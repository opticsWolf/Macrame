//! Lineage identity and its lifecycle (§15.2, §15.4).
//!
//! The read half of branching shipped first, at 0.14.2 through 0.14.6: the
//! ledger tables carry a `branch_id`, [`crate::graph::TraversalBuilder::on_branch`]
//! resolves along the ancestry, and 0.14.6 bounded that resolution by the fork
//! point ([D-223]). Nothing in that half could *create* a lineage — only raw
//! SQL could, which is why 0.14.6 could repair the read's semantics without
//! breaking anyone. This module is the other half: the name, and the one write
//! that registers it.
//!
//! # Why the read shipped first, and why that ordering is not an accident
//!
//! A write that creates something unreadable is the worse order. Had `fork()`
//! landed at 0.14.2, every branch created between then and 0.14.6 would have
//! been readable only through a query that silently absorbed its parent's later
//! writes — and the repair would have been a semantic break on stored data
//! rather than a correction to an unreachable path. That is [D-160] → [D-174]'s
//! ordering, applied a third time and recorded in [D-223].
//!
//! [D-160]: ../../docs/architecture/s13-decision-register.md#d-160
//! [D-174]: ../../docs/architecture/s13-decision-register.md#d-174
//! [D-223]: ../../docs/architecture/s13-decision-register.md#d-223

use crate::error::{DbError, Result};
use crate::schema::ddl;

/// Longest accepted lineage name.
///
/// A sanity bound rather than a schema limit — `branches.branch_id` is `TEXT`
/// and SQLite would take a megabyte. It is set well above every identifier
/// shape the motivating use case generates (a ULID is 26 characters, a
/// hyphenated UUID 36, a path-like `turn/17/alt/3` shorter still) and well
/// below anything that would make a `branches` listing unreadable.
pub const MAX_BRANCH_ID: usize = 128;

/// A validated lineage name.
///
/// # This is not [`ModelName`](crate::vector::ModelName)'s reason, and the rule
/// is deliberately different
///
/// `ModelName` exists because a model name is spliced into a table identifier
/// and SQLite cannot bind an identifier as a parameter, so the validation is
/// what makes the splice safe. **None of that applies here.** A `branch_id`
/// reaches SQL as a bound value at every one of its call sites; there is no
/// splice to protect. Copying `ModelName`'s `[a-z][a-z0-9_]*` rule would be
/// borrowing a justification that does not hold, and it would reject the two
/// name shapes the use case in §15.5 actually produces — a hyphenated UUID and
/// a path-like turn id.
///
/// What this type is for is narrower and has nothing to do with SQL:
///
/// * `branch_id` is a primary key that four ledger tables hold a foreign key
///   into, and `branches` is append-only under two unconditional triggers
///   ([`CREATE_BRANCHES_GUARD_UPDATE`](crate::schema::ddl::CREATE_BRANCHES_GUARD_UPDATE)).
///   A name is therefore written **once** and can never be corrected — not by
///   the crate, not by raw SQL. Validation has one chance, and this is it.
/// * A name that differs from the caller's intent by a trailing space is not a
///   typo, it is a **second lineage that reads as the first**. Every subsequent
///   `on_branch("release ")` resolves to a different ancestry than
///   `on_branch("release")`, both succeed, and neither reports anything. That
///   is the failure shape this wave keeps finding, and it is cheapest to refuse
///   at [D-034]'s boundary.
///
/// So the rule is: non-empty, at most [`MAX_BRANCH_ID`] bytes, no ASCII control
/// characters, and no leading or trailing whitespace. Refused rather than
/// trimmed, on §4.1's principle — a silent repair here becomes two lineages
/// sharing one intent later.
///
/// # `main` is constructible and not forkable
///
/// [`Database::branches`](crate::Database::branches) returns the trunk and
/// every read may name it, so `BranchId::new("main")` must succeed. What is
/// refused is *creating* it a second time, and that refusal belongs to
/// [`Database::fork`](crate::Database::fork) rather than to this type: the
/// trunk is a lineage like any other to a reader, and only a writer has cause
/// to care that it already exists.
///
/// [D-034]: ../../docs/architecture/s13-decision-register.md#d-034
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BranchId(String);

impl BranchId {
    /// Validate `raw` as a lineage name, or explain why it is not one.
    pub fn new(raw: impl AsRef<str>) -> Result<Self> {
        let raw = raw.as_ref();
        let invalid = || DbError::InvalidBranchId(raw.to_string());

        if raw.is_empty() || raw.len() > MAX_BRANCH_ID {
            return Err(invalid());
        }
        if raw.chars().any(|c| c.is_control()) {
            return Err(invalid());
        }
        // `trim` and not `trim_ascii`: a non-breaking space is invisible in
        // every terminal this name will be read in, which is the whole argument
        // above for refusing the ASCII one.
        if raw.trim() != raw {
            return Err(invalid());
        }
        Ok(Self(raw.to_string()))
    }

    /// The trunk, which every database has from its first migration.
    pub fn main() -> Self {
        Self(ddl::MAIN_BRANCH.to_string())
    }

    /// Adopt a name already stored in `branches`, without revalidating it.
    ///
    /// Infallible on purpose. `branches` may hold rows this type never saw:
    /// written by raw SQL, or by a build older than 0.14.7, both of which the
    /// schema permits and neither of which the append-only guards allow anyone
    /// to repair. A listing that returned `Err` on one such row would be
    /// unusable for the one thing it is for — finding out what is in there —
    /// and would report the *listing* as broken rather than the row.
    pub(crate) fn from_stored(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// The name, as it is stored.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this names the trunk.
    pub fn is_main(&self) -> bool {
        self.0 == ddl::MAIN_BRANCH
    }
}

impl std::fmt::Display for BranchId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for BranchId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// So a `BranchId` can be handed straight to the 0.14.4 read surface.
///
/// [`TraversalBuilder::on_branch`](crate::graph::TraversalBuilder::on_branch)
/// and [`query_as_of_edges_on`](crate::temporal::query_as_of_edges_on) take
/// `impl Into<String>`, and they shipped two releases before this type existed.
/// This impl is what makes `on_branch(id)` compile rather than
/// `on_branch(id.as_str())` — additive, and cheaper than widening four
/// signatures that are already public.
impl From<BranchId> for String {
    fn from(id: BranchId) -> Self {
        id.0
    }
}

/// One row of `branches`: a lineage, its parent, and where it was cut.
///
/// # `#[non_exhaustive]` costs nothing here, and that is a fact about direction
///
/// [`EdgeBelief`](crate::temporal::EdgeBelief) needed a constructor to go with
/// the attribute, because `save_snapshot` is public and *takes* one — without
/// `EdgeBelief::new` the attribute would not have made the next field additive,
/// it would have made a public function uncallable (0.14.5). This type only
/// ever travels outward: [`Database::branches`](crate::Database::branches)
/// returns it and nothing public accepts it. So the attribute buys the additive
/// field and takes nothing back, and no constructor is owed. §15.4's
/// abandonment arm is the field it is being kept open for.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub struct Branch {
    /// The lineage's own name.
    pub id: BranchId,
    /// The lineage it was cut from, or `None` for the trunk.
    pub parent: Option<BranchId>,
    /// The transaction-time instant it was cut at, or `None` for the trunk.
    ///
    /// This is the visibility cutoff 0.14.6 reads: the branch sees its parent's
    /// history up to and including this instant, and nothing the parent records
    /// after it ([D-223]).
    ///
    /// [D-223]: ../../docs/architecture/s13-decision-register.md#d-223
    pub forked_at: Option<String>,
    /// When the row was written.
    ///
    /// A separate column from `forked_at` rather than a duplicate of it, and
    /// the two are equal for every branch this release can create. They are
    /// separate so that forking from a *past* instant is an additive change
    /// later: a historical fork has a `forked_at` behind its `created_at`, and
    /// the schema has always allowed it — `CHECK (forked_at <= created_at)`.
    pub created_at: String,
}

/// Register a lineage, refusing the three things a `CHECK` cannot (§15.2).
///
/// Runs inside the write actor, so the three reads and the insert are one turn
/// against one connection and nothing can register a colliding name between the
/// check and the write.
///
/// # What the schema already refuses, and what is left over
///
/// `branches` carries a foreign key on `parent_id`, a primary key on
/// `branch_id`, and two row-local `CHECK`s. So a missing parent and a duplicate
/// name would both fail at the engine anyway — they are checked here to be
/// *named*, because `classify` would otherwise surface a duplicate fork as a
/// constraint violation naming a column, and the caller's question was about a
/// branch.
///
/// # The third refusal, and the invariant that turned out not to be checkable
///
/// [`CREATE_BRANCHES_TABLE`]'s comment left this to `fork()`: *"the fork point
/// is at or after the parent's creation" is not [row-local], and a `CHECK`
/// cannot see another row. The cross-row half is `fork()`'s to enforce at
/// D-034's boundary.* Enforcing it as written **refuses every fork on every
/// injected-clock database in the crate**, and the reason is not the clock the
/// test chose. `seed_root_branch` stamps `main.created_at` from
/// `SystemTime::now()` inside `migrations::run`, which runs *before* the
/// database's clock is resolved — the floor that clock is raised against is
/// read from tables the migration has to create first, so the order cannot
/// simply be swapped. **`branches.created_at` is therefore not on the ledger's
/// timeline**, and on a [`FakeClock`](crate::util::FakeClock) database it sits
/// years in the future of every row.
///
/// What *is* comparable is `forked_at`: every one of them is issued by this
/// function from the same clock as every `recorded_at`. So the check is against
/// the parent's fork point, and the trunk — whose `forked_at` is `NULL` because
/// it was cut from nothing — constrains nothing, which is right: as far as any
/// ledger row is concerned the trunk has always existed.
///
/// The narrower rule is also the one worth having. It makes fork points
/// **non-decreasing down any root path**, which is precisely the property
/// [`ancestry_cte`](crate::graph::lineage) clamps for defensively. The clamp
/// stays — `branches` accepts raw-SQL rows this function never saw — but for
/// rows the crate wrote it is now a belt beside braces rather than the only
/// thing holding the shape.
///
/// What it refuses is a fork point earlier than its parent's. That branch would
/// inherit **nothing whatever from the parent it names** — every row the parent
/// wrote is after the parent's own fork point, so all of them fall past the
/// child's cutoff — leaving a lineage whose `parent_id` says one thing and
/// whose visible history says another. A sibling wearing a child's parent
/// pointer, and silent about it.
///
/// [`CREATE_BRANCHES_TABLE`]: crate::schema::ddl::CREATE_BRANCHES_TABLE
pub(crate) async fn fork(
    conn: &libsql::Connection,
    name: &BranchId,
    parent: &BranchId,
    stamp: &str,
) -> Result<Branch> {
    // `EXISTS` separately from `forked_at`, because the trunk's `forked_at` is
    // legitimately NULL and a single nullable column cannot tell "no such
    // branch" apart from "the root".
    let mut rows = conn
        .query(
            "SELECT (SELECT COUNT(*) FROM branches WHERE branch_id = ?1), \
                    (SELECT COUNT(*) FROM branches WHERE branch_id = ?2), \
                    (SELECT forked_at FROM branches WHERE branch_id = ?2)",
            libsql::params![name.as_str(), parent.as_str()],
        )
        .await?;
    let row = rows.next().await?.ok_or_else(|| {
        // A three-aggregate SELECT always returns a row; if it did not, the
        // honest report is that the parent could not be established.
        DbError::UnknownBranch(parent.as_str().to_string())
    })?;
    let taken: i64 = row.get(0)?;
    let parent_exists: i64 = row.get(1)?;
    let parent_forked_at: Option<String> = row.get(2)?;

    if taken > 0 {
        return Err(DbError::BranchExists(name.as_str().to_string()));
    }
    if parent_exists == 0 {
        return Err(DbError::UnknownBranch(parent.as_str().to_string()));
    }
    if let Some(parent_forked_at) = parent_forked_at {
        if stamp < parent_forked_at.as_str() {
            return Err(DbError::ForkPrecedesParent {
                branch: name.as_str().to_string(),
                parent: parent.as_str().to_string(),
                forked_at: stamp.to_string(),
                parent_forked_at,
            });
        }
    }

    conn.execute(
        "INSERT INTO branches (branch_id, parent_id, forked_at, created_at) \
         VALUES (?1, ?2, ?3, ?3)",
        libsql::params![name.as_str(), parent.as_str(), stamp],
    )
    .await?;

    Ok(Branch {
        id: name.clone(),
        parent: Some(parent.clone()),
        forked_at: Some(stamp.to_string()),
        created_at: stamp.to_string(),
    })
}

/// Every lineage the ledger knows about, trunk first, then by creation.
///
/// Read through the read connection rather than the actor: `branches` is
/// append-only, so a listing cannot be torn by a concurrent write in any way a
/// caller could act on — the worst a racing `fork` can do is not appear yet.
///
/// Ordered rather than left to the engine, and ordered by `created_at` rather
/// than by name, because the useful reading of this list is the shape of the
/// tree over time. The trunk is pinned first because it is the one row that is
/// always there and is nobody's child.
pub(crate) async fn list(conn: &libsql::Connection) -> Result<Vec<Branch>> {
    let mut rows = conn
        .query(
            "SELECT branch_id, parent_id, forked_at, created_at FROM branches \
             ORDER BY (parent_id IS NOT NULL), created_at, branch_id",
            (),
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(Branch {
            id: BranchId::from_stored(row.get::<String>(0)?),
            parent: row.get::<Option<String>>(1)?.map(BranchId::from_stored),
            forked_at: row.get(2)?,
            created_at: row.get(3)?,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_name_shapes_the_use_case_generates() {
        for ok in [
            "main",
            "b9",
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",           // ULID
            "3f2504e0-4f89-11d3-9a0c-0305e82c3301", // hyphenated UUID
            "turn/17/alt/3",                        // path-like turn id
            "explore Kant's second critique",       // a human sentence
        ] {
            assert_eq!(BranchId::new(ok).unwrap().as_str(), ok);
        }
    }

    /// The two that matter are the whitespace pair: each is a second lineage
    /// that reads as the first everywhere it is printed.
    #[test]
    fn refuses_what_cannot_be_corrected_later() {
        for bad in [
            "",
            " release", // leading space
            "release ", // trailing space
            "release\n",
            "rel\tease", // control character in the middle
            "rel\0ease",
        ] {
            assert!(
                BranchId::new(bad).is_err(),
                "{bad:?} was accepted, and `branches` is append-only"
            );
        }
        assert!(BranchId::new("x".repeat(MAX_BRANCH_ID)).is_ok());
        assert!(BranchId::new("x".repeat(MAX_BRANCH_ID + 1)).is_err());
    }

    /// `ModelName`'s rule is the one this type is most likely to be "fixed" to
    /// match. Pinned so the fix has to argue with a test.
    #[test]
    fn the_rule_is_not_model_names_rule() {
        for accepted_here_rejected_there in ["Release-1", "3f2504e0-4f89", "a.b"] {
            assert!(BranchId::new(accepted_here_rejected_there).is_ok());
            assert!(crate::vector::ModelName::new(accepted_here_rejected_there).is_err());
        }
    }

    #[test]
    fn the_trunk_knows_itself() {
        assert!(BranchId::main().is_main());
        assert!(BranchId::new("main").unwrap().is_main());
        assert!(!BranchId::new("mains").unwrap().is_main());
    }
}
