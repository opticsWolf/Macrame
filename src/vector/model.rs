use crate::error::{DbError, Result};

/// A validated embedding-model name, and therefore a safe SQL identifier.
///
/// Per-model tables (D-005) mean the model name reaches SQL as part of a table
/// name — `embeddings_nomic_v1` — and SQLite cannot bind an identifier as a
/// parameter, so the name is necessarily spliced into the statement text. That
/// splice is only safe if the name cannot contain anything but identifier
/// characters, which is what this type establishes once, at the boundary,
/// instead of at each of the several call sites that build a statement.
///
/// Before this existed, `search_vector` built its table name with
/// `format!("embeddings_{model}")` from an unvalidated `&str` argument.
///
/// The rule is deliberately narrower than SQLite's: lowercase ASCII, digits and
/// underscore, first character a letter, 48 characters at most. Rejecting
/// `nomic-v1` rather than quietly rewriting it to `nomic_v1` is the same
/// principle §4.1 applies to timestamps — a silent repair here becomes two
/// models sharing one table later, which is precisely the outcome per-model
/// tables exist to prevent.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModelName(String);

/// Longest accepted model name. SQLite has no meaningful identifier limit; this
/// is a sanity bound so a pathological name cannot produce unreadable DDL.
const MAX_MODEL_NAME: usize = 48;

impl ModelName {
    /// Validate `raw` as a model name, or explain why it is not one.
    pub fn new(raw: impl AsRef<str>) -> Result<Self> {
        let raw = raw.as_ref();
        let invalid = || DbError::InvalidModelName(raw.to_string());

        if raw.is_empty() || raw.len() > MAX_MODEL_NAME {
            return Err(invalid());
        }
        let mut chars = raw.chars();
        match chars.next() {
            Some(c) if c.is_ascii_lowercase() => {}
            _ => return Err(invalid()),
        }
        if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
            return Err(invalid());
        }
        Ok(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The per-model embedding table, `embeddings_<model>`.
    pub fn table(&self) -> String {
        format!("embeddings_{}", self.0)
    }

    /// The DiskANN index over that table's vector column.
    ///
    /// Named, and named predictably, because `vector_top_k` takes the index by
    /// name as a string argument rather than resolving one from the query plan.
    pub fn index(&self) -> String {
        format!("idx_embeddings_{}_vec", self.0)
    }
}

impl std::fmt::Display for ModelName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_names_the_document_uses() {
        for ok in ["nomic_v1", "e5", "bge_m3", "all_minilm_l6_v2", "m1"] {
            assert_eq!(ModelName::new(ok).unwrap().as_str(), ok);
        }
    }

    /// Each of these would otherwise be spliced into a table name.
    #[test]
    fn rejects_anything_that_is_not_a_bare_identifier() {
        for bad in [
            "",
            "nomic-v1",            // hyphen: the obvious near-miss
            "Nomic",               // uppercase: would make table names ambiguous
            "1nomic",              // leading digit
            "_nomic",              // leading underscore
            "nomic v1",            // space
            "nomic\"; DROP TABLE links; --",
            "nomic'",
            "nomic;",
            "nomic(1)",
            "concepts\" --",
            "näh",                 // non-ASCII
        ] {
            assert!(
                matches!(ModelName::new(bad), Err(DbError::InvalidModelName(_))),
                "{bad:?} was accepted as a model name"
            );
        }
        assert!(ModelName::new("a".repeat(MAX_MODEL_NAME + 1)).is_err());
        assert!(ModelName::new("a".repeat(MAX_MODEL_NAME)).is_ok());
    }

    /// The derived identifiers are what actually reaches SQL, so assert their
    /// exact spelling rather than just the name they were built from.
    #[test]
    fn derives_the_table_and_index_identifiers() {
        let m = ModelName::new("nomic_v1").unwrap();
        assert_eq!(m.table(), "embeddings_nomic_v1");
        assert_eq!(m.index(), "idx_embeddings_nomic_v1_vec");
    }

    /// A model table must never be able to collide with a ledger table, or
    /// registering a model would put a vector column where the ledger lives.
    #[test]
    fn no_model_name_can_collide_with_a_ledger_table() {
        for ledger in ["concepts", "links", "links_current", "transaction_log"] {
            assert!(
                ModelName::new(ledger).unwrap().table() != ledger,
                "model {ledger:?} produced a ledger table name"
            );
        }
    }
}
