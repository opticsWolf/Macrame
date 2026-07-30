use crate::error::{DbError, Result};
use crate::util::timestamp::{self, OPEN_SENTINEL};

/// Edge assertion builder for assert / retire / re-assert lifecycle operations.
#[derive(Debug, Clone, PartialEq)]
pub struct EdgeAssertion {
    pub source: String,
    pub target: String,
    pub edge_type: String,
    pub valid_from: String,
    pub valid_to: String,
    pub weight: f64,
    pub properties: String,
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

        for bad in ["", "knows", "KNOWS_WELL", "KNOWS-WELL", "A|B", "O'BRIEN", "ÉTAT"] {
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
