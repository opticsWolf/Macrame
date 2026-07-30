//! Identifiers: what the crate requires of them, and what it merely offers.
//!
//! # The decision (D-061, defects AD and J)
//!
//! **Concept ids are opaque, with two reserved characters.** They are caller
//! data, they are not required to be ULIDs, and the crate stores them as it
//! receives them. What it does require is that an id contain neither `|` nor
//! `/`, because two of the crate's own encodings use those bytes as delimiters
//! and are ambiguous without them reserved.
//!
//! Both of the options the plan offered failed on inspection, and the way they
//! failed is what produced this one.
//!
//! **"Wire `validate_id` into the write path" would have meant requiring
//! ULIDs.** That is not a tightening of the current contract, it is a different
//! contract: every test in the suite uses ids like `a`, `SRC` and `n000`, and so,
//! presumably, does every caller. Three modules *assumed* ULIDs; nothing ever
//! *required* them, and the assumption was wrong rather than merely unenforced.
//!
//! **"Declare ids fully opaque" would have left two encodings ambiguous.** It is
//! stated as needing only a width-independent cycle check, and that is the
//! smaller half. The other half is `transaction_log.entity_id`, which a link
//! writes as `source|target|type|valid_from`. With `|` legal inside a component,
//! two genuinely different links can produce one key — so the log cannot say
//! which relationship a row belongs to, and the fold partition that closed defect
//! W does not help, because both rows are links.
//!
//! So the constraint is the delimiters and nothing else. It is narrow enough to
//! leave every existing id valid, and it turns three assumptions into one
//! enforced property:
//!
//! - `edge_key` / `trg_links_log_insert` — the `|` join is now unambiguous.
//! - The traversal CTE's cycle check — `/`-delimited and no longer dependent on
//!   every id being the same width. See `TraversalBuilder::build_sql`.
//! - Defect W's collision — a concept id can no longer *be* a link key, so the
//!   conflation is unreachable rather than merely harmless. The partition fix
//!   stays as defence in depth.
//!
//! Rejected: percent-encoding the components instead (moves the cost to every
//! read and makes the log unreadable by eye, to buy two characters back);
//! choosing delimiters no id would plausibly contain (the same bet, made
//! silently — "plausibly" is what an assumption is).

use ulid::Ulid;

use crate::error::{DbError, Result};

/// Bytes an identifier may not contain, and where each is load-bearing.
///
/// `|` joins a link's entity key in `transaction_log`. Structural, not
/// cosmetic — a collision produces a wrong answer with no error.
///
/// **`/` was reserved here through 0.5.6 and is free again as of 0.6.0
/// (T0.1, D-076).** It existed for exactly one reason: the traversal CTE
/// carried a `path` column of visited ids, delimited by `/`, and D-061 reserved
/// the character so the cycle check could match a whole element rather than a
/// substring once ids became variable-length. T0.1 deleted the path column —
/// the walk dedupes on entry instead — so nothing in the crate delimits with
/// `/` any more, and continuing to refuse it would be a constraint kept for a
/// mechanism that no longer exists.
///
/// Relaxing is safe in the direction that matters: ids previously refused are
/// now accepted, so no stored id becomes invalid and no migration is implied.
/// Adding a character back later would not be safe, which is why this list is
/// worth keeping short.
pub const RESERVED_ID_CHARS: [char; 1] = ['|'];

/// Generate a new Crockford base32 26-character ULID string.
///
/// **Offered, not required.** Callers with no identifier scheme of their own
/// should use this: ULIDs are sortable, collision-resistant, and contain neither
/// reserved character by construction. Callers who have their own ids keep them,
/// subject only to [`validate_id`].
pub fn generate_id() -> String {
    Ulid::new().to_string()
}

/// Whether `id` is a ULID. Not a requirement — see [`generate_id`].
pub fn is_ulid(id: &str) -> bool {
    Ulid::from_string(id).is_ok()
}

/// Refuse an identifier the crate's own encodings cannot represent (D-061).
///
/// Called from `ConceptUpsert::normalized` and `EdgeAssertion::normalized`, so
/// a bad id is refused at the boundary with a typed error rather than becoming
/// an ambiguous log key three layers down.
///
/// **The error type is the fix for defect J.** This used to return
/// `DbError::NotFound(id)` for a malformed identifier, which says the opposite
/// of what happened — the id was not looked up and not missing; it was refused.
/// A caller matching on `NotFound` to decide whether to create the thing would
/// have been sent to create it again, with the same id, forever.
pub fn validate_id(id: &str) -> Result<()> {
    if id.is_empty() {
        return Err(DbError::InvalidId {
            id: id.to_string(),
            reason: "identifier is empty".to_string(),
        });
    }
    if let Some(c) = id.chars().find(|c| RESERVED_ID_CHARS.contains(c)) {
        return Err(DbError::InvalidId {
            id: id.to_string(),
            reason: format!(
                "identifier contains the reserved character {c:?}; \
                 {RESERVED_ID_CHARS:?} delimits the transaction-log entity key, \
                 so an id carrying one is ambiguous"
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_identifiers_are_accepted() {
        // Every shape the suite and, by implication, callers actually use.
        for id in ["a", "SRC", "n000", "node_100", "c1", &generate_id()] {
            validate_id(id).unwrap_or_else(|e| panic!("{id:?} must stay valid: {e}"));
        }
    }

    /// `/` is accepted again (T0.1, D-076).
    ///
    /// It was reserved solely for the traversal CTE's path delimiter, and the
    /// path column is gone. Asserted rather than assumed, because a constraint
    /// that outlives its mechanism is invisible until someone hits it — and
    /// because the direction matters: this test failing means the crate has
    /// started refusing ids it accepted, which is a breaking change to callers.
    #[test]
    fn a_slash_is_no_longer_reserved() {
        for id in ["a/b", "urn:x/y/z", "2026/01/01"] {
            validate_id(id).unwrap_or_else(|e| panic!("{id:?} must be accepted now: {e}"));
        }
    }

    #[test]
    fn a_reserved_character_is_refused_by_name() {
        for id in ["a|b", "a|b|KNOWS|2026-01-01T00:00:00.000000Z"] {
            let err = validate_id(id).unwrap_err();
            assert!(
                matches!(err, DbError::InvalidId { .. }),
                "{id:?} gave {err:?}"
            );
        }
    }

    /// Defect J: a refused identifier is not a missing one.
    #[test]
    fn a_refusal_is_not_a_not_found() {
        assert!(!matches!(
            validate_id("a|b").unwrap_err(),
            DbError::NotFound(_)
        ));
    }

    /// A ULID must satisfy the constraint, or `generate_id` would hand callers
    /// ids the write path rejects.
    #[test]
    fn generated_ids_are_always_valid() {
        for _ in 0..64 {
            let id = generate_id();
            assert!(is_ulid(&id));
            validate_id(&id).unwrap();
        }
    }
}
