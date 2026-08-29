use crate::branch::BranchId;
use crate::error::{DbError, Result};
use crate::util::timestamp::{self, OPEN_SENTINEL};

/// Edge assertion builder for assert / retire / re-assert lifecycle operations.
///
/// # `#[non_exhaustive]` since 0.14.8, and what that costs
///
/// [`branch`](Self::branch) is the first field added to this struct since it
/// was written, and adding a public field to a struct with all-public fields is
/// already a break: `EdgeAssertion { source, target, .. }` as a literal stops
/// compiling. Taking `#[non_exhaustive]` in the same release converts a break
/// that will recur into one that happens once — the builder
/// ([`new`](Self::new) and the setters) is the documented path, is what every
/// caller in this crate and its bindings uses, and keeps working untouched.
/// [`EdgeBelief`](crate::temporal::EdgeBelief) took the same treatment at
/// 0.14.5 for the same reason (D-222).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct EdgeAssertion {
    pub source: String,
    pub target: String,
    pub edge_type: String,
    pub valid_from: String,
    pub valid_to: String,
    pub weight: f64,
    pub properties: String,
    /// The lineage this assertion is made on, or `None` for the trunk (§15.4,
    /// D-225).
    ///
    /// `None` and `Some(BranchId::main())` name the same lineage and are not
    /// distinguished by the write, which is deliberate: `main` is a branch like
    /// any other and a caller who spells it out should get exactly what a
    /// caller who left it unset gets. What `None` buys is on the *cost* side —
    /// the write path can take the pre-0.14.8 statement, with no branch
    /// existence check and no second parameter, so a database that never forks
    /// pays nothing for a column it cannot vary. See
    /// [`Database::assert_edge`](crate::Database::assert_edge).
    pub branch: Option<BranchId>,
}

impl EdgeAssertion {
    pub fn new(
        source: impl Into<String>,
        target: impl Into<String>,
        edge_type: impl Into<String>,
    ) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
            edge_type: edge_type.into(),
            valid_from: String::new(),
            valid_to: OPEN_SENTINEL.to_string(),
            weight: 1.0,
            properties: "{}".to_string(),
            branch: None,
        }
    }

    /// When the asserted fact starts being true (valid time, Doctrine II).
    pub fn valid_from(mut self, ts: impl Into<String>) -> Self {
        self.valid_from = ts.into();
        self
    }

    /// When the asserted fact stops being true. Defaults to the open sentinel.
    pub fn valid_to(mut self, ts: impl Into<String>) -> Self {
        self.valid_to = ts.into();
        self
    }

    pub fn weight(mut self, weight: f64) -> Self {
        self.weight = weight;
        self
    }

    pub fn properties(mut self, json: impl Into<String>) -> Self {
        self.properties = json.into();
        self
    }

    /// Assert this edge on `branch` rather than on the trunk (0.14.8, §15.4).
    ///
    /// The same name the read side takes
    /// ([`TraversalBuilder::on_branch`](crate::graph::TraversalBuilder::on_branch)),
    /// because it is the same question asked of the other half: *which lineage
    /// is this about*. Until 0.14.8 only the read could ask it, and a caller who
    /// forked and then asserted got a successful write **on the trunk** — the
    /// gap [`Database::fork`](crate::Database::fork)'s rustdoc has named since
    /// 0.14.7 and this closes.
    ///
    /// # This writes a row *beside* the ancestor's, never over it
    ///
    /// `links_current` is keyed `(source_id, target_id, edge_type, valid_from,
    /// branch_id)`, so an assertion on a branch about an edge it inherited adds
    /// the branch's own row and leaves the parent's untouched. That is the
    /// whole storage cost of divergence, and it is what makes the parent's
    /// history unchanged by anything a branch does —
    /// [Doctrine III](../../docs/architecture/s0-s3-foundations.md#doctrine-iii)
    /// is not a policy the write path enforces here, it is a shape the key
    /// makes unrepresentable.
    ///
    /// The read resolves the two by nearest lineage
    /// ([D-220](../../docs/architecture/s13-decision-register.md#d-220)), so the
    /// branch sees its own and the trunk keeps seeing the trunk's.
    pub fn on_branch(mut self, branch: BranchId) -> Self {
        self.branch = Some(branch);
        self
    }

    /// The lineage this assertion names, spelled out.
    ///
    /// One place that decides what `None` means, so the insert, the overlap
    /// guard and the existence check cannot answer it three ways.
    pub(crate) fn branch_name(&self) -> &str {
        self.branch
            .as_ref()
            .map_or(crate::schema::ddl::MAIN_BRANCH, BranchId::as_str)
    }

    /// Check the assertion and put its timestamps in canonical form (D-029).
    ///
    /// Runs before the write reaches the actor, so a malformed edge type or a
    /// second-precision timestamp comes back as a typed error rather than as an
    /// engine `CHECK` failure from the other side of a channel — by which point
    /// the caller has lost the context that would explain it.
    pub fn normalized(mut self) -> Result<Self> {
        // Both endpoints, because the log's entity key concatenates both and an
        // ambiguity in either makes the row unattributable (D-061).
        crate::util::ids::validate_id(&self.source)?;
        crate::util::ids::validate_id(&self.target)?;
        validate_edge_type(&self.edge_type)?;
        self.valid_from = timestamp::normalize(&self.valid_from)?;
        self.valid_to = timestamp::normalize(&self.valid_to)?;
        Ok(self)
    }
}

/// Edge types are `[A-Z0-9]+` (§4.1).
///
/// The constraint is not cosmetic: edge types are concatenated into
/// `transaction_log.entity_id` with `|` separators, so a type containing a
/// separator would corrupt the key that replay reads the log back by.
///
/// This comment used to also claim the constraint protected the traversal CTE,
/// which spliced edge types in as quoted literals. It did not: this function
/// runs from [`EdgeAssertion::normalized`] on the write path only, and
/// [`super::TraversalBuilder::edge_types`] never called it. The CTE now binds
/// them as parameters (D-039), so that half of the justification is gone rather
/// than merely unenforced.
pub fn validate_edge_type(edge_type: &str) -> Result<()> {
    let ok = !edge_type.is_empty()
        && edge_type
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    if ok {
        Ok(())
    } else {
        Err(DbError::InvalidEdgeType(edge_type.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_types_are_uppercase_alphanumeric() {
        assert!(validate_edge_type("KNOWS").is_ok());
        assert!(validate_edge_type("REL2").is_ok());

        for bad in [
            "",
            "knows",
            "KNOWS_WELL",
            "KNOWS-WELL",
            "A|B",
            "O'BRIEN",
            "ÉTAT",
        ] {
            assert!(
                validate_edge_type(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn normalizing_widens_timestamps_and_rejects_bad_types() {
        let e = EdgeAssertion::new("a", "b", "KNOWS")
            .valid_from("2026-01-01T00:00:00Z")
            .normalized()
            .unwrap();
        assert_eq!(e.valid_from, "2026-01-01T00:00:00.000000Z");
        assert_eq!(e.valid_to, OPEN_SENTINEL);

        assert!(EdgeAssertion::new("a", "b", "bad")
            .valid_from("2026-01-01T00:00:00.000000Z")
            .normalized()
            .is_err());
    }
}
