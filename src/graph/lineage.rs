//! Reading one lineage's belief out of a ledger that holds several (§15.2,
//! §15.3, D-219, D-220, D-223).
//!
//! # Three shapes, and the measurements that forced them
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
//! The third shape is the trunk's, on a database that has forked
//! ([`LineageShape::TrunkOnForked`], 0.15.2, D-244). Once `branches` holds
//! two rows the trunk's read was `Resolved` like everyone else's, and paid
//! the hybrid's fixed cost — D-223 measured 1.45× at zero churn, and the
//! trunk has zero churn *structurally*: it has no ancestors, so no cutoff
//! and no churned set. Its resolved read reduces to its own rows, and the
//! third shape emits that reduction as one predicate on `branch_id`. It is
//! the escalation D-223 named — the naive filter, emitted when the answer
//! to *does any ancestor hold a post-cutoff row* is no — taken where the
//! answer is no by construction rather than by probing.
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
//! deliberately **not** guarded by `check_recorded_reach`, and the reason has
//! changed shape without changing sides (0.15.4, D-246). It used to be that the
//! guard's bit was coarse — any archive at all flipped it, so guarding here
//! would have refused every branched read on every archived database. The guard
//! is now scoped to the instants the archive really took, which removes that
//! objection and leaves the smaller one: `links_cut` reads the log for a *fork
//! point*, not for a belief, and a fork point that has gone cold is a different
//! question from an instant that has. A cold arm for `links_cut` is the fix if
//! it is ever needed, and it belongs with §3.2 rather than with the cutoff.

use crate::error::{DbError, Result};
use crate::graph::plan::{lower, Resolution};
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
    /// More than one lineage exists and the reader is a **root** — the
    /// trunk, on every database this crate can write (0.15.2, D-244).
    ///
    /// A root has no ancestors, so its ancestry is one row with no cutoff,
    /// its churned set is empty by construction, and the resolved read
    /// reduces exactly to *its own rows*: `links_current WHERE branch_id =
    /// ?` under current belief, and the fold over its own log entries at a
    /// recorded instant. This shape emits that reduction directly. It is
    /// the third shape [D-223] named as the escalation — "the naive filter
    /// emitted when no ancestor holds a post-cutoff row" — taken at the
    /// one lineage for which the condition is structural rather than
    /// measured, and it is what stops the trunk paying for the branches
    /// (review C-7). It **binds** the branch, unlike `Trunk`, because its
    /// SQL names it.
    ///
    /// [D-223]: ../../docs/architecture/s13-decision-register.md#d-223
    TrunkOnForked,
}

impl LineageShape {
    /// Whether the emitted SQL names the reading branch at all.
    ///
    /// `Trunk` is the one shape that does not: there is one lineage and
    /// nothing to name. Every placeholder layout that puts the branch at a
    /// slot asks this rather than comparing against `Resolved`, so that a
    /// shape added later binds correctly by default.
    pub(crate) fn binds_branch(self) -> bool {
        !matches!(self, LineageShape::Trunk)
    }
}

/// The shape, and the ancestry the shape needs — one round trip for both.
///
/// This is what `lineage_shape` became (0.15.17, [D-259]). Where that asked
/// SQLite for three aggregates, this loads the rows and answers from them;
/// measured, that is **10.0 µs against 11.0**
/// (`examples/ancestry_resolve_probe.rs` §5), so the ancestry arrives for less
/// than the shape alone used to cost and nothing has to be cached to afford it.
///
/// The ancestry is empty under the two trunk shapes. Neither emits a `lineage`
/// relation, so resolving one would be a walk whose result no SQL names — and
/// an empty slice is what makes [`crate::graph::plan::lower`] able to ignore
/// the field rather than branch on whether it was populated.
///
/// A caller wanting the question and not the answer goes through [`Lineages`]
/// directly: `branch::diff` loads the register once and asks [`Lineages::shape`]
/// per name, which refuses an unregistered lineage before either side is
/// lowered — and is one round trip for both, where a free function per name was
/// two.
///
/// [D-259]: ../../docs/architecture/s13-decision-register.md#d-259
pub(crate) async fn resolve_for(
    conn: &libsql::Connection,
    branch: Option<&str>,
) -> Result<(LineageShape, Vec<Ancestor>)> {
    let named = branch.unwrap_or(ddl::MAIN_BRANCH);
    let lineages = Lineages::load(conn).await?;
    let shape = lineages.shape(named)?;
    let ancestry = match shape {
        LineageShape::Resolved => lineages.ancestry(named),
        _ => Vec::new(),
    };
    Ok((shape, ancestry))
}

/// One lineage's row in `branches`, as the resolution needs it.
///
/// Three columns and not the table: `created_at` is the audit column and no
/// read resolves over it, so loading it would be bytes moved for nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BranchRow {
    pub(crate) id: String,
    pub(crate) parent: Option<String>,
    pub(crate) forked_at: Option<String>,
}

/// Every lineage the database holds — the table [`resolve`] walks.
///
/// A `Vec` and a linear scan, for the reason `distinct_branches` gives: the
/// bound is the number of lineages, which is small and human-authored, and a
/// map would allocate to index a list that is usually of length one.
///
/// # Why this is loaded per read rather than cached (0.15.17, [D-259])
///
/// [A-2] proposed a cached `Vec<Branch>` with a generation counter, on the
/// premise that resolving ancestry in Rust needs the *rows* where the shape
/// needed only three aggregates, and that the extra read has to be paid for.
/// Measured (`examples/ancestry_resolve_probe.rs`, §5), it does not: loading 17
/// rows costs **10.0 µs** against the three-aggregate `SELECT`'s **11.0 µs**,
/// so the rows arrive for *less* than the answer they replace. The cache is an
/// optimisation nothing has asked for, and a cache the read side does not need
/// is a coherency question the read side does not have to answer.
///
/// The actor keeps its copy ([`crate::connection`]'s `ActorState`) because the
/// write path already had one and invalidates it on the two commands that write
/// `branches`. That is a cache with an owner; this is not.
///
/// [A-2]: ../../docs/Macrame%20Codebase%20Review%20v0.15.0.md
/// [D-259]: ../../docs/architecture/s13-decision-register.md#d-259
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Lineages {
    pub(crate) rows: Vec<BranchRow>,
}

impl Lineages {
    /// Load `branches`, in no particular order.
    ///
    /// Unordered because every consumer here searches by name or walks parent
    /// links, and neither cares; `Database::branches` is the listing that
    /// promises trunk-first and it sorts for itself.
    pub(crate) async fn load(conn: &libsql::Connection) -> Result<Self> {
        let mut rows = conn
            .query("SELECT branch_id, parent_id, forked_at FROM branches", ())
            .await?;
        let mut out = Vec::with_capacity(1);
        while let Some(row) = rows.next().await? {
            out.push(BranchRow {
                id: row.get::<String>(0)?,
                parent: row.get::<Option<String>>(1)?,
                forked_at: row.get::<Option<String>>(2)?,
            });
        }
        Ok(Self { rows: out })
    }

    fn find(&self, name: &str) -> Option<&BranchRow> {
        self.rows.iter().find(|b| b.id == name)
    }

    /// The shape for one name, or [`DbError::UnknownBranch`].
    ///
    /// The same three facts `lineage_shape`'s `SELECT` asked for until 0.15.17 — the total,
    /// the name's existence, and whether it is a root — read off the rows
    /// instead of counted by SQLite.
    /// # Refusing an unregistered lineage rather than answering for the trunk
    ///
    /// A read naming a branch that is not in `branches` has asked a question
    /// about something that does not exist. Answering it with the trunk's view
    /// would be the [D-069] failure — a right-looking answer to a question that
    /// was not asked — and it is the answer a caller is *least* able to detect,
    /// because on a database that has never forked the trunk's view is what
    /// they expected to see anyway.
    ///
    /// The refusal is [`DbError::UnknownBranch`] from 0.14.7 and was
    /// [`DbError::NotFound`] before it, whose `Display` reads *"node {0} not
    /// found"* — the wrong noun, pointing a caller at their concept ids. There
    /// was no better variant until `fork()` needed one, and shipping the right
    /// variant while leaving this on the old one would have been two spellings
    /// of one fact.
    ///
    /// [D-069]: ../../docs/architecture/s13-decision-register.md
    pub(crate) fn shape(&self, name: &str) -> Result<LineageShape> {
        let row = self
            .find(name)
            .ok_or_else(|| DbError::UnknownBranch(name.to_string()))?;
        Ok(if self.rows.len() <= 1 {
            LineageShape::Trunk
        } else if row.parent.is_none() {
            LineageShape::TrunkOnForked
        } else {
            LineageShape::Resolved
        })
    }

    /// The shape a batch takes, given every lineage it names.
    ///
    /// Every name is checked, because that is the first thing the caller wants:
    /// a batch naming a lineage that does not exist is refused **by name**,
    /// rather than by whatever the guard finds when it looks in the wrong place.
    ///
    /// # Why the last answer used to be every answer, and why it is not one now
    ///
    /// Before 0.15.2 the shape was a function of the row *count* alone, so
    /// asking per name and keeping the last was correct and read like a bug —
    /// review C-24. [`LineageShape::TrunkOnForked`] (D-244) made the shape a
    /// function of the **name** as well: a root and a fork on one database now
    /// have different shapes, and the loop went on keeping whichever came last.
    /// Where the names disagree the resolved form is the one exact for all of
    /// them, roots included, so the ambiguity is resolved rather than
    /// tie-broken by iteration order.
    pub(crate) fn shape_of(&self, names: &[&str]) -> Result<LineageShape> {
        let mut agreed: Option<LineageShape> = None;
        for name in names {
            let shape = self.shape(name)?;
            agreed = Some(match agreed {
                None => shape,
                Some(prev) if prev == shape => prev,
                Some(_) => LineageShape::Resolved,
            });
        }
        // No names is the trunk: `distinct_branches` never returns empty, and
        // `write_concepts_atomic` refuses an empty chunk before it gets here.
        Ok(agreed.unwrap_or(LineageShape::Trunk))
    }

    /// [`resolve`], against these rows.
    pub(crate) fn ancestry(&self, start: &str) -> Vec<Ancestor> {
        resolve(&self.rows, start)
    }
}

/// One resolved ancestor: exactly the three columns the recursive
/// `ancestry_cte` produced until 0.15.17.
///
/// Public through [`crate::branch`] as the answer `reconstruct_on` resolves
/// over, which is review C-10: until 0.15.17 the resolution rule existed only
/// as SQL, so a caller holding a `Vec<EdgeBelief>` had no function to finish
/// the question the fold started.
///
/// # Usually read, occasionally built (0.15.17, [D-255])
///
/// `#[non_exhaustive]`, and [`new`](Ancestor::new) is the way in — a fourth
/// column is plausible (the fork's own `created_at` has been wanted twice) and
/// a struct literal outside this crate would make it a major version.
///
/// Almost every caller *reads* one: [`Database::ancestry`] resolves it out of
/// `branches`, refusing an unregistered lineage first, and
/// [`resolve_beliefs`](crate::temporal::resolve_beliefs) consumes what it
/// returned. The constructor exists for the caller who is exercising that pure
/// function rather than a database, which is a real case — it is what this
/// crate's own test for it does — and is worth naming, because an ancestry
/// assembled by hand is a distance rule the caller has stated and
/// `resolve_beliefs` will apply as faithfully as it applies a resolved one.
///
/// [D-255]: ../../docs/architecture/s13-decision-register.md#d-255
/// [`Database::ancestry`]: crate::Database::ancestry
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Ancestor {
    /// The lineage.
    pub branch_id: String,
    /// Steps from the reader: 0 is the reader itself.
    pub dist: i64,
    /// The instant past which this ancestor's rows are not visible, or `None`
    /// for the reader, which has no cutoff.
    pub cutoff: Option<String>,
}

impl Ancestor {
    /// One ancestor at `dist` steps up the parent chain, uncut.
    ///
    /// `cutoff` is `None`, which is the reader's own row and the only shape
    /// with no fork point above it; [`cutoff`](Ancestor::cutoff) sets it for
    /// the rest. Two arguments and a setter rather than three arguments,
    /// because the third is an `Option` that is `None` on exactly one row of
    /// every ancestry and a positional `None` at every call site reads as a
    /// decision nobody made.
    ///
    /// Building one states a distance rule rather than reporting a resolved
    /// one. [`crate::Database::ancestry`] is what to use against a real ledger.
    pub fn new(branch_id: impl Into<String>, dist: i64) -> Self {
        Self {
            branch_id: branch_id.into(),
            dist,
            cutoff: None,
        }
    }

    /// Cut this ancestor at `ts`.
    pub fn cutoff(mut self, ts: impl Into<String>) -> Self {
        self.cutoff = Some(ts.into());
        self
    }
}

/// Walk `branches` from `start` to its root, carrying the running minimum.
///
/// The Rust half of [D-259]: the same relation [`ancestry_cte`] computed with
/// `WITH RECURSIVE`, computed here instead. Term for term — the reader at
/// `dist` 0 with no cutoff, then one row per ancestor, each carrying the
/// **minimum** `forked_at` seen on the path down to it.
///
/// See [`ancestry_cte`] for why the cutoff is the *child's* `forked_at` and why
/// the minimum is running rather than assigned. The property that matters here
/// is that this is a second implementation of a rule the crate already had, so
/// it is pinned against the original differentially rather than against a
/// restatement of the rule (`the_rust_walk_agrees_with_the_cte`, below).
///
/// # Termination without trusting the data
///
/// `branches.parent_id` is a foreign key into an append-only table whose parent
/// must exist before its child, so the graph is a forest and the walk is finite.
/// The loop is bounded by the row count anyway. That bound is not the
/// termination argument — it is what stops a corrupted file from hanging a read,
/// which is a different failure from the one the schema rules out, and the walk
/// returns the prefix it had rather than raising: a caller reading a broken
/// `branches` gets a narrower answer, not a panic.
///
/// [D-259]: ../../docs/architecture/s13-decision-register.md#d-259
pub(crate) fn resolve(rows: &[BranchRow], start: &str) -> Vec<Ancestor> {
    let mut out = Vec::new();
    let mut cur = start.to_string();
    let mut cutoff: Option<String> = None;
    for dist in 0..=(rows.len() as i64) {
        out.push(Ancestor {
            branch_id: cur.clone(),
            dist,
            cutoff: cutoff.clone(),
        });
        let Some(node) = rows.iter().find(|b| b.id == cur) else {
            break;
        };
        // The CHECK pairs these two, so a row with one and not the other is a
        // file that has been edited outside this crate. Treated as the root.
        let (Some(parent), Some(forked)) = (node.parent.as_ref(), node.forked_at.as_ref()) else {
            break;
        };
        cutoff = Some(match cutoff {
            Some(c) if c.as_str() <= forked.as_str() => c,
            _ => forked.clone(),
        });
        cur = parent.clone();
    }
    out
}

/// The ancestry as a bound `VALUES` table, replacing the recursive CTE.
///
/// `first_slot` is where the block starts; it occupies `3 × rows` placeholders
/// from there, and every reader puts it **after** its own fixed slots so that no
/// existing layout moves.
///
/// # Bound and not interpolated
///
/// A branch id is caller-supplied text. The crate has exactly one arbitrary-SQL
/// surface ([D-258]) and this is not a second one, so every value binds — the
/// cutoff too, `NULL` included, which libSQL accepts inside `VALUES`
/// (`ancestry_resolve_probe.rs` §1 checks it rather than assuming it). The
/// consequence is that the statement *text* varies with ancestry **depth** and
/// with nothing else, so a prepared-statement cache keyed on text sees one entry
/// per distinct fork depth rather than one per lineage.
///
/// `dist` binds too, though it is the row's own index and could be a literal.
/// Measured (`ancestry_resolve_probe.rs` §7) that saves a third of the
/// placeholders and **1–5%**, inside the noise, so it is spelled the way the
/// other two columns are rather than differently for a gain that did not
/// survive being measured.
///
/// [D-258]: ../../docs/architecture/s13-decision-register.md#d-258
pub(crate) fn ancestry_values(rows: usize, first_slot: usize, tag: &str) -> String {
    let tuples: Vec<String> = (0..rows)
        .map(|i| {
            let b = first_slot + i * 3;
            format!("(?{}, ?{}, ?{})", b, b + 1, b + 2)
        })
        .collect();
    format!(
        "lineage{tag}(branch_id, dist, cutoff) AS (VALUES {})",
        tuples.join(", ")
    )
}

/// The ancestry's placeholder values, in the order [`ancestry_values`] names
/// them.
pub(crate) fn ancestry_params(ancestry: &[Ancestor]) -> Vec<libsql::Value> {
    let mut v = Vec::with_capacity(ancestry.len() * 3);
    for a in ancestry {
        v.push(libsql::Value::Text(a.branch_id.clone()));
        v.push(libsql::Value::Integer(a.dist));
        v.push(match &a.cutoff {
            Some(c) => libsql::Value::Text(c.clone()),
            None => libsql::Value::Null,
        });
    }
    v
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
///
/// # `tag`, and the one query that needs two of these (0.14.11, [D-228])
///
/// Every read before `diff` resolved **one** lineage, so the four CTEs could
/// take their names as constants. A diff resolves two in one statement — it
/// has to, because two statements compare two snapshots and can report a
/// difference that never existed — and SQLite has one namespace per `WITH`
/// list. So each name here takes a suffix, and every caller that resolves one
/// lineage passes `""` and emits exactly the text it emitted before.
///
/// A suffix rather than a second copy of the hybrid, for the reason
/// [D-227](../../docs/architecture/s13-decision-register.md#d-227) gave when it
/// declined to hand-write the cutoff into `query_as_of_edges_on`: the two arms
/// of [`links_cut_cte`] must partition, and that is a property of one
/// comparison written once, not an argument to restate in a second place.
///
/// [D-228]: ../../docs/architecture/s13-decision-register.md#d-228
/// The three placeholders that fix one edge key, when the reader has one.
///
/// The write path always does: [`crate::connection`]'s overlap guard and its
/// retirement both hold a `(source, target, edge_type)` before any SQL exists,
/// and asking the resolution about the whole projection to then discard all but
/// one key would make a branched bulk write O(rows) per row.
///
/// Narrowing is **not** a filter appended to the resolved relation. It is
/// pushed into the base scans of [`churned_cte`] and [`links_cut_cte`], where
/// `idx_lc_open_interval` leads with exactly these three columns and each arm
/// becomes a seek (0.14.8, [D-225]; lowered 0.15.8, W13.3, [D-250]).
///
/// It also decides one column. A keyed read is a *write* path read, and the
/// write path carries `properties` through the resolution because
/// [`retire_from_resolved`] restates it on the shadow row; an
/// unkeyed traversal does not, and carrying a JSON blob through a window over
/// the whole projection is not a cost a reader should pay for a column it never
/// selects.
///
/// [D-225]: ../../docs/architecture/s13-decision-register.md#d-225
/// [D-250]: ../../docs/architecture/s13-decision-register.md#d-250
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KeySlots {
    pub source: usize,
    pub target: usize,
    pub edge_type: usize,
}

impl KeySlots {
    /// The three equalities, on `alias`, as one line of a `WHERE`.
    pub(crate) fn equalities(&self, alias: &str) -> String {
        format!(
            "{alias}.source_id = ?{} AND {alias}.target_id = ?{} AND {alias}.edge_type = ?{}",
            self.source, self.target, self.edge_type
        )
    }
}

/// # Retained as the oracle, not as production SQL (0.15.17, [D-259])
///
/// Nothing emits this any more — [`ancestry_values`] does, with the same three
/// columns computed by [`resolve`] instead of by SQLite. It is compiled for
/// tests only, and it stays because deleting it would leave
/// [`resolve`] pinned against a *restatement* of the rule rather than against
/// the implementation it replaced. `the_rust_walk_agrees_with_the_cte` is the
/// differential test; this is the half of it that cannot be wrong by the same
/// mistake.
///
/// [D-259]: ../../docs/architecture/s13-decision-register.md#d-259
#[cfg(test)]
pub(crate) fn ancestry_cte(slot: usize, tag: &str) -> String {
    format!(
        r#"lineage{tag}(branch_id, dist, cutoff) AS (
    SELECT ?{slot}, 0, NULL
    UNION ALL
    SELECT b.parent_id, g.dist + 1,
           CASE WHEN g.cutoff IS NULL OR b.forked_at < g.cutoff
                THEN b.forked_at ELSE g.cutoff END
    FROM branches b JOIN lineage{tag} g ON b.branch_id = g.branch_id
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
pub(crate) fn churned_cte(tag: &str, key: Option<KeySlots>) -> String {
    // Keyed, the three equalities go first and the base scan becomes a seek on
    // `idx_lc_open_interval`. Composed from the *columns* either way rather
    // than from the key's own placeholders: the rows are already narrowed to
    // that key, so the two spellings hold the same string, and one of them is
    // the spelling the unkeyed arm has to use anyway.
    let narrow = match key {
        Some(k) => format!("{} AND ", k.equalities("lc")),
        None => String::new(),
    };
    format!(
        r#"churned{tag}(entity_id, branch_id, cutoff) AS (
    SELECT lc.source_id || '|' || lc.target_id || '|' || lc.edge_type || '|' || lc.valid_from,
           lc.branch_id, g.cutoff
    FROM links_current lc
    JOIN lineage{tag} g ON g.branch_id = lc.branch_id
    WHERE {narrow}g.cutoff IS NOT NULL AND lc.recorded_at > g.cutoff
)"#
    )
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
pub(crate) fn links_cut_cte(tag: &str, key: Option<KeySlots>) -> String {
    // The narrowing goes on the **projection** arm only. The log arm joins
    // `churned{tag}`, which is already one key when this one is, so repeating
    // the equalities there would narrow nothing and read as though it did.
    //
    // The parentheses around the cutoff disjunction are the whole reason this
    // is built rather than concatenated: `A AND B OR C` is `(A AND B) OR C` in
    // SQL, so appending the key to this arm without them would return every
    // ancestor's pre-cutoff row for every edge in the ledger.
    let (narrow, props, log_props) = match key {
        Some(k) => (
            format!(
                "{} AND (g.cutoff IS NULL OR lc.recorded_at <= g.cutoff)",
                k.equalities("lc")
            ),
            ", properties",
            "\n           json_extract(payload, '$.properties'),",
        ),
        None => (
            "g.cutoff IS NULL OR lc.recorded_at <= g.cutoff".to_string(),
            "",
            "",
        ),
    };
    let carried = if key.is_some() {
        "lc.weight, lc.properties, lc.branch_id"
    } else {
        "lc.weight, lc.branch_id"
    };
    format!(
        r#"links_cut{tag}(source_id, target_id, edge_type, valid_from, valid_to, weight{props}, branch_id) AS (
    SELECT lc.source_id, lc.target_id, lc.edge_type, lc.valid_from, lc.valid_to,
           {carried}
    FROM links_current lc
    JOIN lineage{tag} g ON g.branch_id = lc.branch_id
    WHERE {narrow}
    UNION ALL
    SELECT json_extract(payload, '$.source_id'),
           json_extract(payload, '$.target_id'),
           json_extract(payload, '$.edge_type'),
           json_extract(payload, '$.valid_from'),
           json_extract(payload, '$.valid_to'),
           json_extract(payload, '$.weight'),{log_props}
           branch_id
    FROM (
        SELECT transaction_log.payload, transaction_log.branch_id,
               ROW_NUMBER() OVER (
                   PARTITION BY transaction_log.entity_id, transaction_log.branch_id
                   ORDER BY transaction_log.seq_id DESC
               ) AS rn
        FROM transaction_log
        JOIN churned{tag} k ON k.entity_id = transaction_log.entity_id
                      AND k.branch_id = transaction_log.branch_id
        WHERE transaction_log.table_name = 'links'
          AND transaction_log.recorded_at <= k.cutoff
    ) WHERE rn = 1
)"#
    )
}

/// The slots the write path binds, which are fixed by its two statements
/// rather than by a layout type (0.14.8, D-225).
///
/// `?1` source, `?2` target, `?3` edge type, `?4` the `valid_from` each tail
/// selects on, `?5` the writing lineage, and for the retirement `?6` the new
/// `valid_to` and `?7` the stamp. Both callers bind all of them positionally
/// at one site each.
const WRITE_KEY: KeySlots = KeySlots {
    source: 1,
    target: 2,
    edge_type: 3,
};
const WRITE_BRANCH_SLOT: usize = 5;

/// Where the guard's ancestry block starts: after the five it already binds.
///
/// The two write statements have different layouts, so the slot is passed to
/// [`write_resolution`] rather than named once here — the guard binds five
/// (`key`, `valid_from`, `branch`) and the retirement binds seven.
const GUARD_ANCESTRY_SLOT: usize = 6;

/// Where the retirement's ancestry block starts: after `valid_to` and `stamp`.
const RETIRE_ANCESTRY_SLOT: usize = 8;

/// What the write path has decided before any SQL exists.
///
/// Current belief always: the guard asks what this lineage believes *now*, and
/// an assertion is made against now. There is no recorded slot to name.
fn write_resolution<'a>(
    shape: LineageShape,
    ancestry: &'a [Ancestor],
    ancestry_slot: usize,
) -> Resolution<'a> {
    Resolution {
        shape,
        branch_slot: WRITE_BRANCH_SLOT,
        recorded_slot: None,
        tag: "",
        key: Some(WRITE_KEY),
        ancestry,
        ancestry_slot,
    }
}

/// Overlap candidates as the writing lineage can see them (0.14.8, §15.4,
/// [D-225]; lowered 0.15.8, W13.3, [D-250]).
///
/// # Why the write path needs a resolution at all
///
/// [`crate::connection`]'s overlap guard reads `links_current` for the edge key
/// being asserted and refuses an assertion whose valid-time interval overlaps
/// one already recorded (defect AA, D-060). Until 0.14.8 every row in the table
/// was `main`'s, so reading the key with no lineage predicate was exact. The
/// moment a second lineage can write, the same statement is wrong in **both
/// directions at once**: a branch would be refused for overlapping its parent's
/// belief that it is entitled to supersede, and the trunk would be refused for
/// overlapping a branch's belief it cannot even see. An unfiltered read is not
/// a conservative approximation of a filtered one here; it is a different
/// question.
///
/// Adding `AND branch_id = ?` would fix the trunk direction and leave the
/// branch one wrong the other way — a branch would then be checked against
/// *only its own* rows and could assert `[10,20)` over an inherited `[5,15)`,
/// putting two overlapping intervals into its own view. That is defect AA
/// reintroduced across lineages, and it is the shape
/// `trg_links_single_open`'s v12 comment left open as "§15.4's write-path
/// question": the trigger sees one row and cannot answer it. This is the
/// answer. **What a lineage may not overlap is what that lineage can see**,
/// which is the read's definition and now the write's.
///
/// # It was a second spelling of that definition until 0.15.8
///
/// The narrowing was real and is unchanged — [`churned_cte`] and
/// [`links_cut_cte`] are written for a traversal and scan `links_current`
/// whole, so calling them per assertion would make a branched bulk write
/// O(rows) per row, and pushing the key into the base scans turns each arm
/// into a seek on `idx_lc_open_interval`. What was not real was the *copy*:
/// a `key_visibility_cte` holding its own `lineage`, its own churned set, its
/// own two-arm hybrid and its own nearest-lineage window, four relations that
/// had to keep agreeing with four in [`crate::graph::plan`] and were kept
/// honest only by `branch_write_tests` asserting the two answers match on one
/// fixture. [D-227](../../docs/architecture/s13-decision-register.md#d-227) is
/// four releases of what happens when a reader spells its own. The key is a
/// [`KeySlots`] on [`Resolution`] now, and this function is the lowering plus
/// one line.
///
/// # What that cost, and bought, on `examples/edge_write_probe`
///
/// Best of 500 `assert_edge` calls, three runs each, release build:
///
/// ```text
///           0.15.7    0.15.8
/// trunk    0.0958    0.0966 ms   unchanged
/// forked   0.1044    0.0975 ms   -6.6%
/// branch   0.1059    0.1091 ms   +3.0%
/// ```
///
/// The forked trunk gains because it stopped taking the resolved form: it was
/// exact there only because a root's ancestry is itself, and D-248's C-24
/// repair is what lets [`crate::graph::LineageShape`] tell a root apart from a
/// branch at all. It now lowers to a two-predicate lookup on `links_current`.
///
/// The branch loses because the shared [`visible_cte`] joins `lineage` to order
/// by `dist`, where the deleted `key_visibility_cte` carried `dist` through its
/// own relations and needed no join. Buying that 3% back means giving the keyed
/// spelling its own `dist` column in three functions — a second shape for the
/// hybrid, decided by the caller — which is the divergence this release exists
/// to remove, over a join against a materialised ancestry of two rows.
///
/// **The `churned` base scan is unchanged and was never the cost.** It planned
/// as `SEARCH lc USING COVERING INDEX idx_lc_lineage_cut (branch_id=? AND
/// recorded_at>?)` in 0.15.7 and it plans that way now: SQLite inlines the
/// key-narrowed CTE into each use, so the equalities and the `recorded_at`
/// range meet in one scan either way. Splitting them apart with
/// `AS MATERIALIZED` does restore the key seek, and costs more than it saves —
/// the log arm then loses `SEARCH transaction_log USING INDEX idx_txlog_entity
/// (entity_id=?)` and scans the whole log, because a materialised `churned` is
/// no longer a small driving set the planner can see through. It was measured
/// and not taken.
///
/// # The predicate set, which is three equalities and nothing else
///
/// `valid_from <> ?4` excludes the row being re-asserted: re-assertion at the
/// same `valid_from` is Doctrine III's ordinary case and is settled by the
/// primary key and the single-open trigger, not by this guard.
///
/// **The "and nothing else" was measured, not assumed**, and it is the half of
/// this statement a lowering must not quietly improve. The first version added
/// `AND valid_from < :new_valid_to`, a provably safe narrowing — overlap
/// requires `max(start) < min(end)`, so an interval starting at or after the
/// new one's end cannot overlap it. It cost **9.8 ms on a 90-edge chunk into a
/// 2,000-edge hub**, because it walked the planner straight into D-059's trap:
///
/// ```text
/// with the range:     SEARCH links_current USING COVERING INDEX
///                     idx_lc_traversal_cover (source_id=? AND valid_from<?)
/// without it:         SEARCH links_current USING COVERING INDEX
///                     idx_lc_open_interval (source_id=? AND target_id=? AND edge_type=?)
/// ```
///
/// `idx_lc_traversal_cover` leads on `(source_id, valid_from, …)` and contains
/// every column that query mentions, so with a `valid_from` range available it
/// wins as a covering index while binding **one** equality column — and the
/// guard scans the source's entire out-degree. Same shape as the defect D-059
/// diagnosed in `trg_links_single_open`, reintroduced by an optimisation one
/// wave after it was fixed. Three equalities make it a point lookup that
/// `idx_lc_open_interval` serves exactly, and the rows it returns are a version
/// count rather than an out-degree. **A narrowing predicate is not free if it
/// changes the plan** — which is also why [`KeySlots`] pushes its equalities
/// into the base scans rather than appending them to the resolved relation,
/// and why `index_plan_tests` pins this statement's plan on every shape.
///
/// [D-225]: ../../docs/architecture/s13-decision-register.md#d-225
/// [D-250]: ../../docs/architecture/s13-decision-register.md#d-250
pub(crate) fn overlap_candidates_resolved(shape: LineageShape, ancestry: &[Ancestor]) -> String {
    let l = lower(&write_resolution(shape, ancestry, GUARD_ANCESTRY_SLOT));
    format!(
        "{}SELECT l.valid_from, l.valid_to FROM {} l WHERE l.valid_from <> ?4{}",
        l.with_clause(),
        l.source,
        l.filter
    )
}

/// The row a branch is retiring, which may belong to an ancestor.
///
/// Retirement on a branch is **shadow retirement**: the branch writes its own
/// row at the ancestor's key carrying a closed interval, the read prefers it by
/// `dist`, and the ancestor's row is untouched. [`visible_cte`]'s rustdoc has
/// described this write since 0.14.4; this is it. Closing the ancestor's own
/// row is the parent corruption
/// [Doctrine III](../../docs/architecture/s0-s3-foundations.md#doctrine-iii)
/// forbids, and it is not merely avoided by policy — `links` is append-only and
/// there is no statement in the crate that could do it.
///
/// `?6` is the new `valid_to`, `?7` the stamp. `weight` and `properties` are
/// carried from the visible row rather than restated, which is what makes this
/// a retirement rather than a new assertion that happens to be closed — and it
/// is the reason a keyed resolution carries `properties` at all (see
/// [`KeySlots`]).
pub(crate) fn retire_from_resolved(shape: LineageShape, ancestry: &[Ancestor]) -> String {
    let l = lower(&write_resolution(shape, ancestry, RETIRE_ANCESTRY_SLOT));
    format!(
        "{}INSERT INTO links \
             (source_id, target_id, edge_type, valid_from, valid_to, weight, \
              properties, recorded_at, branch_id) \
         SELECT ?1, ?2, ?3, ?4, ?6, l.weight, l.properties, ?7, ?5 \
         FROM {} l WHERE l.valid_from = ?4{}",
        l.with_clause(),
        l.source,
        l.filter
    )
}

/// What one lineage believes and another does not, in **one** statement
/// (0.14.11, §15.4, [D-228]).
///
/// # Why one statement rather than two reads and a difference in Rust
///
/// Not for the round trip. A diff is a *comparison*, and two statements
/// against [`Database::read_conn`](crate::Database::read_conn) are two
/// snapshots — a write landing between them can make the answer report a
/// difference that never existed at any instant. The obvious repair, a read
/// transaction, is not available: `read_conn()` is public and shared, so
/// beginning one inside a library call would change what every other holder of
/// that connection sees. One statement gets the single snapshot for free.
///
/// # Why the CTEs are tagged rather than copied
///
/// This resolves two lineages, so it needs two of each of the four CTEs, and
/// SQLite has one namespace per `WITH` list. Hence the `tag` parameter on
/// [`ancestry_cte`] and its three companions rather than a second spelling of
/// the hybrid — which [D-227](../../docs/architecture/s13-decision-register.md#d-227)
/// declined for `query_as_of_edges_on` and would be the same mistake here.
/// Every single-lineage caller passes `""` and emits the text it always did.
///
/// # What it compares, and what it does not
///
/// A `LEFT JOIN` on the **edge key** — `(source, target, type, valid_from)` —
/// and a row survives when `b` holds no belief about that key, or holds one
/// whose interval or weight differs. So a retirement is reported: `a`'s row is
/// the closed one, `b`'s is open, and they differ. That is the case a
/// valid-time filter would have hidden, which is why there is no `ts` here at
/// all — a diff filtered to an instant cannot see the one divergence that is
/// *about* an instant having passed.
///
/// **`properties` is not compared**, and that is a limit rather than an
/// oversight: no read surface in the crate returns edge properties —
/// `EdgeAssertion::properties` writes them and nothing reads them back — so a
/// diff reporting a change there would be the only reader resolving a column,
/// and it would name a difference the caller has no way to look at. It is the
/// first thing to widen if edge properties ever become readable.
///
/// Float equality on `weight` is deliberate. The question is whether `b` holds
/// the *same belief*, and a belief is a stored value; an epsilon here would
/// invent a tolerance the ledger does not have and would make `diff(a, b)`
/// disagree with what a traversal on either lineage shows.
///
/// Slots: `?1` the lineage being asked about, `?2` the one it is compared to.
///
/// [D-228]: ../../docs/architecture/s13-decision-register.md#d-228
pub(crate) fn diff_sql(a_ancestry: &[Ancestor], b_ancestry: &[Ancestor]) -> String {
    // Two lowerings in one `WITH` list (0.15.1, W13.1): the same prelude
    // the traversal and `query_as_of_edges_on` splice, told apart by tag.
    //
    // Two ancestry blocks as well, and they are the reason this function stopped
    // being a constant in 0.15.17: `?1` and `?2` still name the two lineages,
    // `a`'s ancestry follows at `?3`, and `b`'s follows that. Both lengths are
    // read from the database, so the caller resolves before it lowers.
    let a = lower(&Resolution {
        shape: LineageShape::Resolved,
        branch_slot: 1,
        recorded_slot: None,
        tag: "_a",
        key: None,
        ancestry: a_ancestry,
        ancestry_slot: 3,
    });
    let b = lower(&Resolution {
        shape: LineageShape::Resolved,
        branch_slot: 2,
        recorded_slot: None,
        tag: "_b",
        key: None,
        ancestry: b_ancestry,
        ancestry_slot: 3 + a_ancestry.len() * 3,
    });
    format!(
        "WITH RECURSIVE {},\n{}\n\
         SELECT a.source_id, a.target_id, a.edge_type, a.valid_from, \
                a.valid_to, a.weight, a.branch_id \
         FROM {} a \
         LEFT JOIN {} b ON b.source_id  = a.source_id \
                              AND b.target_id  = a.target_id \
                              AND b.edge_type  = a.edge_type \
                              AND b.valid_from = a.valid_from \
         WHERE b.source_id IS NULL \
            OR b.valid_to <> a.valid_to \
            OR b.weight   <> a.weight \
         ORDER BY a.source_id, a.target_id, a.edge_type, a.valid_from",
        a.with_list(),
        b.with_list(),
        a.source,
        b.source,
    )
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
pub(crate) fn visible_cte(source: &str, tag: &str, key: Option<KeySlots>) -> String {
    // The partition stays the whole edge key even when three quarters of it is
    // a constant: it is the same rows either way, and a partition that changed
    // shape with the caller would be a second definition of what one edge is.
    let props = if key.is_some() { ", properties" } else { "" };
    let carried = if key.is_some() {
        "l.properties, l.branch_id,"
    } else {
        "l.branch_id,"
    };
    format!(
        r#"visible{tag}(source_id, target_id, edge_type, valid_from, valid_to, weight{props}, branch_id) AS (
    SELECT source_id, target_id, edge_type, valid_from, valid_to, weight{props}, branch_id FROM (
        SELECT l.source_id, l.target_id, l.edge_type, l.valid_from, l.valid_to, l.weight,
               {carried}
               ROW_NUMBER() OVER (
                   PARTITION BY l.source_id, l.target_id, l.edge_type, l.valid_from
                   ORDER BY g.dist
               ) AS rn
        FROM {source} l
        JOIN lineage{tag} g ON g.branch_id = l.branch_id
    ) WHERE rn = 1
)"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reader plus one ancestor: the smallest ancestry that resolves
    /// anything. An empty one lowers to `VALUES ()`, which SQLite refuses.
    fn anc() -> Vec<Ancestor> {
        vec![
            Ancestor {
                branch_id: "exp".to_string(),
                dist: 0,
                cutoff: None,
            },
            Ancestor {
                branch_id: "main".to_string(),
                dist: 1,
                cutoff: Some("2026-01-06T00:00:00.000000Z".to_string()),
            },
        ]
    }

    const TS: &str = "2026-01-06T00:00:00.000000Z";

    async fn fresh() -> libsql::Connection {
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        crate::schema::run_migrations(&conn).await.unwrap();
        conn
    }

    async fn fork(conn: &libsql::Connection, child: &str, parent: &str) {
        conn.execute(
            "INSERT INTO branches (branch_id, parent_id, forked_at, created_at) \
             VALUES (?1, ?2, ?3, ?3)",
            libsql::params![child, parent, TS],
        )
        .await
        .unwrap();
    }

    /// **The guard seeks the edge key on every shape it can be given.**
    ///
    /// D-060's overlap guard fell into D-059's trap one wave after D-059 fixed
    /// it: a provably safe `AND valid_from < :new_valid_to` handed the planner
    /// a range, `idx_lc_traversal_cover` won as a covering index while binding
    /// **one** column, and the guard scanned the source's whole out-degree —
    /// **+9.8 ms** on a 90-edge chunk into a 2,000-edge hub, invisible to every
    /// correctness test because the rows returned were right.
    ///
    /// Until 0.15.8 that was pinned in `migration_tests` against a *hand-copied*
    /// reproduction of `OVERLAP_CANDIDATES`, which is the weakest form of this
    /// test: it pins a string next to the code rather than the code. The
    /// statements are generated now ([`overlap_candidates_resolved`],
    /// [`retire_from_resolved`]), so the plan is taken from the bytes the guard
    /// will prepare — on all three shapes, including the two the old pin could
    /// not reach because it predated them.
    ///
    /// **Three columns bound is the assertion**, not the index name: the
    /// resolved shape reaches `links_current` through four CTEs and the trunk
    /// shapes reach it directly, so which index serves the seek differs, and
    /// what must not differ is that the seek is on the whole key. One column
    /// bound is O(out-degree) wherever it appears.
    #[tokio::test]
    async fn the_guard_seeks_the_edge_key_on_every_shape() {
        let conn = fresh().await;
        fork(&conn, "exp", "main").await;

        for shape in [
            LineageShape::Trunk,
            LineageShape::TrunkOnForked,
            LineageShape::Resolved,
        ] {
            for (what, sql) in [
                ("overlap", overlap_candidates_resolved(shape, &anc())),
                ("retire", retire_from_resolved(shape, &anc())),
            ] {
                let mut rows = conn
                    .query(&format!("EXPLAIN QUERY PLAN {sql}"), ())
                    .await
                    .unwrap();
                let mut plan = Vec::new();
                while let Some(r) = rows.next().await.unwrap() {
                    plan.push(r.get::<String>(3).unwrap());
                }
                let step = plan.join(" | ");

                assert!(
                    step.contains("source_id=? AND target_id=? AND edge_type=?"),
                    "{shape:?}/{what} binds fewer columns than the key has, so \
                     it scans the source's out-degree — D-059 in D-060's \
                     guard: {step}"
                );
                // The overlap read has no `valid_from` equality to fall back
                // on, so it is the one that needs the index built for it.
                assert!(
                    what != "overlap" || step.contains("idx_lc_open_interval"),
                    "{shape:?} overlap is off the index added for it: {step}"
                );
            }
        }
    }

    /// One row in `branches` is the trunk shape whatever name is asked for,
    /// and an unknown name is refused before any shape is chosen.
    #[tokio::test]
    async fn one_lineage_is_the_trunk_shape() {
        let conn = fresh().await;
        assert_eq!(
            resolve_for(&conn, None).await.unwrap().0,
            LineageShape::Trunk
        );
        assert_eq!(
            resolve_for(&conn, Some("main")).await.unwrap().0,
            LineageShape::Trunk
        );
        assert!(matches!(
            resolve_for(&conn, Some("ghost")).await,
            Err(DbError::UnknownBranch(name)) if name == "ghost"
        ));
    }

    /// Once the ledger has forked, the root reads as itself and every other
    /// lineage resolves (0.15.2, D-244).
    #[tokio::test]
    async fn a_forked_ledger_gives_the_root_its_own_shape() {
        let conn = fresh().await;
        fork(&conn, "b1", "main").await;
        fork(&conn, "b2", "b1").await;
        assert_eq!(
            resolve_for(&conn, None).await.unwrap().0,
            LineageShape::TrunkOnForked,
            "an unbranched read on a forked ledger is the trunk's own read"
        );
        assert_eq!(
            resolve_for(&conn, Some("main")).await.unwrap().0,
            LineageShape::TrunkOnForked
        );
        assert_eq!(
            resolve_for(&conn, Some("b1")).await.unwrap().0,
            LineageShape::Resolved
        );
        assert_eq!(
            resolve_for(&conn, Some("b2")).await.unwrap().0,
            LineageShape::Resolved
        );
        assert!(matches!(
            resolve_for(&conn, Some("ghost")).await,
            Err(DbError::UnknownBranch(_))
        ));
    }

    /// Read the ancestry back from whichever relation produced it, `dist` first.
    async fn ancestry_from(conn: &libsql::Connection, sql: &str) -> Vec<Ancestor> {
        let mut rows = conn.query(sql, ()).await.unwrap();
        let mut out = Vec::new();
        while let Some(r) = rows.next().await.unwrap() {
            out.push(Ancestor {
                branch_id: r.get::<String>(0).unwrap(),
                dist: r.get::<i64>(1).unwrap(),
                cutoff: r.get::<Option<String>>(2).unwrap(),
            });
        }
        out.sort_by_key(|a| a.dist);
        out
    }

    /// The CTE's answer for `start`, through the oracle.
    ///
    /// The branch is spliced rather than bound because this is test-only code
    /// reading a name this test wrote; the production form binds, which is what
    /// [`ancestry_values`] is about.
    async fn from_the_cte(conn: &libsql::Connection, start: &str) -> Vec<Ancestor> {
        let sql = format!(
            "WITH RECURSIVE {} SELECT branch_id, dist, cutoff FROM lineage",
            ancestry_cte(1, "").replace("?1", &format!("'{start}'"))
        );
        ancestry_from(conn, &sql).await
    }

    /// The Rust walk's answer for `start`, round-tripped through the SQL it
    /// generates — so this checks [`resolve`] *and* [`ancestry_values`].
    async fn from_the_values(conn: &libsql::Connection, start: &str) -> Vec<Ancestor> {
        let lineages = Lineages::load(conn).await.unwrap();
        let ancestry = lineages.ancestry(start);
        let sql = format!(
            "WITH {} SELECT branch_id, dist, cutoff FROM lineage",
            ancestry_values(ancestry.len(), 1, "")
        );
        let mut rows = conn.query(&sql, ancestry_params(&ancestry)).await.unwrap();
        let mut out = Vec::new();
        while let Some(r) = rows.next().await.unwrap() {
            out.push(Ancestor {
                branch_id: r.get::<String>(0).unwrap(),
                dist: r.get::<i64>(1).unwrap(),
                cutoff: r.get::<Option<String>>(2).unwrap(),
            });
        }
        out.sort_by_key(|a| a.dist);
        out
    }

    async fn fork_at(conn: &libsql::Connection, name: &str, parent: &str, at: &str) {
        conn.execute(
            "INSERT INTO branches (branch_id, parent_id, forked_at, created_at) \
             VALUES (?1, ?2, ?3, ?3)",
            libsql::params![name, parent, at],
        )
        .await
        .unwrap();
    }

    /// **The differential test this release exists to pass** ([D-259]).
    ///
    /// [`resolve`] replaced a recursive CTE, and the way that goes wrong is not
    /// a crash: it is a cutoff that is one step too wide, on a lineage nobody
    /// reads for a month. So the Rust walk is checked against the SQL it
    /// replaced rather than against a restatement of the rule — every lineage
    /// on every shape below, both answers, byte for byte.
    ///
    /// Four shapes, and the third is the one that matters. A **chain** with
    /// increasing fork points never fires the running minimum. A chain with
    /// *decreasing* ones fires it at every step. Those two are the pair that
    /// tells a `min` apart from a plain assignment, which is the mistake a
    /// hand-written walk actually makes.
    ///
    /// [D-259]: ../../docs/architecture/s13-decision-register.md#d-259
    #[tokio::test]
    async fn the_rust_walk_agrees_with_the_cte() {
        for (label, forks) in [
            // A chain whose fork points increase down it: the minimum never bites.
            (
                "increasing chain",
                vec![
                    ("b1", "main", "2026-01-01T00:00:00.000000Z"),
                    ("b2", "b1", "2026-02-01T00:00:00.000000Z"),
                    ("b3", "b2", "2026-03-01T00:00:00.000000Z"),
                    ("b4", "b3", "2026-04-01T00:00:00.000000Z"),
                ],
            ),
            // The same chain with the fork points running the other way. Every
            // step must clamp to the earliest seen, and a walk that assigns
            // instead of taking a minimum widens each ancestor's window.
            (
                "decreasing chain",
                vec![
                    ("b1", "main", "2026-04-01T00:00:00.000000Z"),
                    ("b2", "b1", "2026-03-01T00:00:00.000000Z"),
                    ("b3", "b2", "2026-02-01T00:00:00.000000Z"),
                    ("b4", "b3", "2026-01-01T00:00:00.000000Z"),
                ],
            ),
            // Siblings: two lineages off one parent, so the walk must not carry
            // a cutoff sideways.
            (
                "siblings",
                vec![
                    ("l", "main", "2026-01-01T00:00:00.000000Z"),
                    ("r", "main", "2026-06-01T00:00:00.000000Z"),
                    ("ll", "l", "2026-02-01T00:00:00.000000Z"),
                ],
            ),
            // A root with nothing under it: the ancestry is one row, no cutoff.
            (
                "lone fork",
                vec![("only", "main", "2026-01-01T00:00:00.000000Z")],
            ),
        ] {
            let conn = fresh().await;
            for (name, parent, at) in &forks {
                fork_at(&conn, name, parent, at).await;
            }
            let mut names = vec!["main".to_string()];
            names.extend(forks.iter().map(|(n, _, _)| n.to_string()));

            for name in &names {
                let cte = from_the_cte(&conn, name).await;
                let values = from_the_values(&conn, name).await;
                assert_eq!(cte, values, "{label}: the two forms disagree for `{name}`");
                // Not vacuous: the reader is always there, and a lineage that
                // is not the trunk always has at least one ancestor.
                assert_eq!(
                    cte.first().map(|a| a.branch_id.as_str()),
                    Some(name.as_str())
                );
                assert!(cte.first().is_some_and(|a| a.cutoff.is_none()));
            }
        }
    }

    /// The clamp, stated directly, so a failure says which rule broke.
    ///
    /// [`the_rust_walk_agrees_with_the_cte`] would catch a dropped minimum, but
    /// it would report it as "the two forms disagree" — true, and one step away
    /// from what went wrong. This names it.
    #[tokio::test]
    async fn the_cutoff_is_the_earliest_fork_on_the_path_and_not_the_nearest() {
        let conn = fresh().await;
        fork_at(&conn, "early", "main", "2026-01-01T00:00:00.000000Z").await;
        fork_at(&conn, "late", "early", "2026-09-01T00:00:00.000000Z").await;

        let lineages = Lineages::load(&conn).await.unwrap();
        let ancestry = lineages.ancestry("late");

        // `late` sees `early` up to the point *it* diverged, and `main` up to
        // the point `early` diverged — the earlier of the two, not the nearer.
        assert_eq!(ancestry[0].cutoff, None, "the reader has no cutoff");
        assert_eq!(
            ancestry[1].cutoff.as_deref(),
            Some("2026-09-01T00:00:00.000000Z"),
            "`early` is seen up to where `late` left it"
        );
        assert_eq!(
            ancestry[2].cutoff.as_deref(),
            Some("2026-01-01T00:00:00.000000Z"),
            "`main` is clamped to where `early` left it, not to where `late` did"
        );
    }

    /// A `branches` table that cannot be walked returns a prefix, not a hang.
    ///
    /// The schema makes this unreachable — `parent_id` is a foreign key into an
    /// append-only table — so this stages it by writing a row the schema would
    /// refuse, with foreign keys off. The bound in [`resolve`] is what stops a
    /// corrupted file from hanging a read, and a bound nothing tests is a
    /// comment.
    #[tokio::test]
    async fn a_parent_that_is_not_there_ends_the_walk() {
        let conn = fresh().await;
        conn.execute("PRAGMA foreign_keys = OFF", ()).await.unwrap();
        fork_at(&conn, "orphan", "vanished", "2026-01-01T00:00:00.000000Z").await;

        let lineages = Lineages::load(&conn).await.unwrap();
        let ancestry = lineages.ancestry("orphan");
        // The orphan, then the parent it names — which no row describes, so the
        // walk stops there rather than looking for its parent.
        assert_eq!(ancestry.len(), 2);
        assert_eq!(ancestry[1].branch_id, "vanished");
    }

    /// The `VALUES` text names three placeholders per ancestor, from the slot
    /// it was given, and nothing before it.
    #[test]
    fn the_bound_ancestry_starts_where_the_reader_put_it() {
        let sql = ancestry_values(2, 6, "");
        assert!(sql.contains("(?6, ?7, ?8)"), "{sql}");
        assert!(sql.contains("(?9, ?10, ?11)"), "{sql}");
        assert!(
            !sql.contains("?5"),
            "nothing below the slot it was given: {sql}"
        );
        // Tagged, for the diff's two lowerings in one `WITH` list.
        assert!(ancestry_values(1, 1, "_a").starts_with("lineage_a("));
    }

    /// Only `Trunk` leaves the branch unbound; the layouts ask this rather
    /// than comparing against `Resolved`.
    #[test]
    fn every_shape_but_the_trunk_binds_the_branch() {
        assert!(!LineageShape::Trunk.binds_branch());
        assert!(LineageShape::Resolved.binds_branch());
        assert!(LineageShape::TrunkOnForked.binds_branch());
    }
}
