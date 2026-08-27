pub(crate) mod audit;
pub(crate) mod rebuild;
pub(crate) mod shadow;

pub use audit::audit_current;
pub use rebuild::{rebuild_current, RebuildReport};
pub use shadow::{ShadowOutcome, ShadowStep};

/// The latest-belief projection of `links` — **the single definition** (T0.2).
///
/// [Doctrine VI](../../docs/architecture/s0-s3-foundations.md) says
/// `links_current` must equal this at all times: one row per interval key
/// `(source_id, target_id, edge_type, valid_from)`, the one with the greatest
/// `recorded_at`. `rebuild_current` makes it so and `audit_current` checks it.
///
/// It lived in both files, byte-for-byte, until 0.6.0. That is the failure class
/// [D-035](../../docs/architecture/s13-decision-register.md#d-035) exists to
/// prevent — a rule stated twice is a rule that can disagree with itself — and
/// it had a second consequence nobody had named: the audit that `rebuild_within`
/// ran on itself was, in effect, a runtime check that the two copies still
/// agreed, paid on every archive, under the archive's write lock, forever. With
/// one definition that check is tautological by construction rather than by
/// assumption, which is what made it safe to stop paying for it (D-077).
///
/// `rn` is not projected: callers select the eight ledger columns.
pub(crate) const LATEST_BELIEF_PROJECTION: &str = r#"
    SELECT source_id, target_id, edge_type, valid_from,
           valid_to, weight, properties, recorded_at
    FROM (
        SELECT source_id, target_id, edge_type, valid_from,
               valid_to, weight, properties, recorded_at,
               ROW_NUMBER() OVER (
                   PARTITION BY source_id, target_id, edge_type, valid_from
                   ORDER BY recorded_at DESC
               ) AS rn
        FROM links
    ) WHERE rn = 1
"#;
