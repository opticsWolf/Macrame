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
///
/// # It is a generator, and the restriction goes *inside* (0.15.3, D-245)
///
/// Three callers want the projection of a *part* of `links` — the shadow
/// rebuild's chunk, its catch-up pass, and the archive's keyed repair — and
/// all three want the restriction inside the subquery. Outside it the window
/// still ranks every partition in the table and the restricted query costs
/// what the whole rebuild costs: correct, and pointless, which is the kind of
/// thing only a benchmark finds. `projection_where` is therefore the one
/// definition and [`latest_belief_projection`] is its unrestricted case;
/// `shadow.rs` carried its own copy of this shape until 0.15.3, which is
/// [D-035](../../docs/architecture/s13-decision-register.md#d-035)'s failure
/// class sitting in the file whose header explains that failure class.
pub(crate) fn projection_where(clause: &str) -> String {
    format!(
        r#"
    SELECT source_id, target_id, edge_type, valid_from,
           valid_to, weight, properties, recorded_at, branch_id
    FROM (
        SELECT source_id, target_id, edge_type, valid_from,
               valid_to, weight, properties, recorded_at, branch_id,
               ROW_NUMBER() OVER (
                   PARTITION BY source_id, target_id, edge_type, valid_from, branch_id
                   ORDER BY recorded_at DESC
               ) AS rn
        FROM links
        WHERE {clause}
    ) WHERE rn = 1
"#
    )
}

/// The projection of the whole table: [`projection_where`] with a clause that
/// admits every row.
///
/// `1 = 1` rather than an `Option<&str>` arm that omits the `WHERE`, because a
/// generator with two shapes is two things to keep true and SQLite discards a
/// constant-true term before it plans anything. `audit_current` and
/// `rebuild_within` are its callers, and both compare or insert whole tables.
pub(crate) fn latest_belief_projection() -> String {
    projection_where("1 = 1")
}
