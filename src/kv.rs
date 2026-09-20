//! `kv_store`: operational state, outside the ledger entirely (§4.9, [D-280]).
//!
//! Hashes, epochs, counters and cursors are neither belief nor content. They
//! need durability and a backup story and have no history worth keeping, so
//! they get a plain `WITHOUT ROWID` key/value table with **no log trigger, no
//! archive membership and no `branch_id`** —
//! [Doctrine VII](../../docs/architecture/s0-s3-foundations.md#doctrine-vii)'s
//! reasoning about embeddings applied to a different derivative.
//!
//! # The store is branch-global, and that is a semantic
//!
//! No `branch_id` means one cursor, one epoch, one counter across every
//! lineage, unchanged by working on a branch, because operational state
//! describes the *process* rather than anything the ledger believes. Nothing in
//! the crate trips over that — a branch is a row in `branches` plus a label
//! carried on writes, so there is no physical copy and no path that enumerates
//! tables to duplicate them. An application storing per-branch state here is
//! storing it in the wrong place; the remedy is to put the branch name in the
//! key, and the key convention is where that would be said.
//!
//! # What carries it, stated exactly
//!
//! A snapshot is a zstd `bincode` [`crate::temporal::MaterializedState`] in a
//! snapshots *directory* and contains no KV, so what carries this table is a
//! **file-level backup**, while transaction-time reconstruction ignores it
//! entirely. That distinction is the whole of the exclusion argument, and it is
//! spelled out here because the draft that proposed the table stated it the
//! other way round.
//!
//! [D-280]: ../../docs/architecture/s13-decision-register.md#d-280

use crate::error::{DbError, Result};

/// The longest key `kv_store` accepts ([D-280]).
///
/// [D-280]: ../../docs/architecture/s13-decision-register.md#d-280
pub const MAX_KV_KEY: usize = 256;

/// Check a key against the store's rule: non-empty, `[A-Za-z0-9_:./\-]+`, at
/// most [`MAX_KV_KEY`] characters.
///
/// The charset is the edge-kind charset of [D-279] plus `/`, because the shapes
/// these keys take are paths and namespaced names — `okf:filehash:notes/a.md`,
/// `myapp:epoch`. It is checked at the API boundary and **not** in the file:
/// unlike `updated_at`, which `canonical_ts_check!` freezes on disk because
/// lexicographic comparison depends on it, nothing in the crate compares keys
/// for anything but equality and prefix, so a raw writer putting an odd key
/// there breaks a convention rather than an invariant.
///
/// # Case is not normalized, and the table is case-sensitive
///
/// `kv_store.key` is a `TEXT PRIMARY KEY` under BINARY collation, so
/// `okf:Epoch` and `okf:epoch` are two rows. No single-case rule is imposed the
/// way [D-279] imposes one on edge kinds, and the difference is deliberate: an
/// edge kind is a vocabulary term a reader must be able to tell apart at a
/// glance in a log line, while a key is an application's own identifier that
/// never appears in the ledger at all.
///
/// [D-279]: ../../docs/architecture/s13-decision-register.md#d-279
pub fn validate_kv_key(key: &str) -> Result<()> {
    if key.is_empty() {
        return Err(DbError::InvalidKvKey(key.to_string()));
    }
    validate_kv_prefix(key)
}

/// [`validate_kv_key`]'s rule with the non-empty clause dropped.
///
/// An empty prefix is a legal argument to [`scan`] — it means *everything*,
/// bounded by the caller's `limit` like every other read here — while an empty
/// key is not addressable at all.
pub fn validate_kv_prefix(prefix: &str) -> Result<()> {
    let ok = prefix.len() <= MAX_KV_KEY
        && prefix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b':' | b'.' | b'/' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(DbError::InvalidKvKey(prefix.to_string()))
    }
}

/// Write a key, replacing whatever was there.
///
/// Plain overwrite semantics and no history: a versioned KV would re-create the
/// ledger through the back door, for state whose previous value nobody wants.
///
/// `stamp` comes from the actor's clock, the same one every write on this
/// connection uses, so `updated_at` is comparable with every other timestamp in
/// the file — which is the property `canonical_ts_check!` on the column makes
/// true of the disk rather than only of this function.
pub(crate) async fn put(
    conn: &libsql::Connection,
    key: &str,
    value: &str,
    stamp: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO kv_store (key, value, updated_at) VALUES (?1, ?2, ?3) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, \
         updated_at = excluded.updated_at",
        libsql::params![key, value, stamp],
    )
    .await?;
    Ok(())
}

/// Read one key.
pub(crate) async fn get(conn: &libsql::Connection, key: &str) -> Result<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT value FROM kv_store WHERE key = ?1",
            libsql::params![key],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

/// Remove one key, reporting whether there was one.
///
/// **This is a physical delete, and it is not a Doctrine V violation**, because
/// Doctrine V is about the ledger and this table is not in it — no log trigger,
/// no archive membership, nothing that could later be asked to explain the
/// absence. The `bool` is returned rather than collapsed into `Ok(())` for the
/// reason [`crate::CheckpointReport`] states at greater length: a delete that
/// removed a row and a delete that found nothing are different answers, and the
/// difference is usually why the caller asked.
pub(crate) async fn delete(conn: &libsql::Connection, key: &str) -> Result<bool> {
    let n = conn
        .execute("DELETE FROM kv_store WHERE key = ?1", libsql::params![key])
        .await?;
    Ok(n > 0)
}

/// Every key under `prefix`, in key order, at most `limit` of them.
///
/// # The bound is an argument, not an `Option`
///
/// `limit` is required on the same reasoning as every other bounded read in
/// this crate: an unbounded scan of a table whose size is an application's
/// business is a stall waiting for the database that grew.
///
/// # Why the predicate has two halves
///
/// `key >= :prefix` is what the primary-key index seeks on, and
/// `key < :prefix_end` is what stops it — without the second half the seek runs
/// to the end of the table and `limit` is the only thing bounding it, which
/// turns a prefix scan into a full scan whenever the matching range is short.
/// The end is the prefix with its last byte incremented, which is exact under
/// BINARY collation and safe because [`validate_kv_prefix`] admits no byte
/// above `b'z'`.
///
/// An empty prefix has no end to compute and means *everything*; it binds
/// `NULL` and the upper half of the predicate stands down.
pub(crate) async fn scan(
    conn: &libsql::Connection,
    prefix: &str,
    limit: usize,
) -> Result<Vec<(String, String)>> {
    let end = prefix_end(prefix);
    let mut rows = conn
        .query(
            "SELECT key, value FROM kv_store \
             WHERE key >= ?1 AND (?2 IS NULL OR key < ?2) \
             ORDER BY key LIMIT ?3",
            libsql::params![prefix, end, limit as i64],
        )
        .await?;

    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push((row.get(0)?, row.get(1)?));
    }
    Ok(out)
}

/// The first string that sorts above every string beginning with `prefix`.
///
/// `None` for an empty prefix, which has no such string.
fn prefix_end(prefix: &str) -> Option<String> {
    let mut bytes = prefix.as_bytes().to_vec();
    // `validate_kv_prefix` caps the charset at `b'z'` (0x7A), so the increment
    // cannot overflow and the result is still ASCII.
    *bytes.last_mut()? += 1;
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_admit_paths_and_namespaces() {
        for good in [
            "okf:epoch",
            "okf:filehash:notes/a.md",
            "myapp.cursor",
            "a_b-c",
            "OKF:EPOCH",
            &"k".repeat(MAX_KV_KEY),
        ] {
            assert!(validate_kv_key(good).is_ok(), "{good:?} should be ok");
        }

        for bad in [
            "",
            "has space",
            "has|pipe",
            "café",
            &"k".repeat(MAX_KV_KEY + 1),
        ] {
            assert!(validate_kv_key(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    /// The empty prefix is the one input the two validators disagree on.
    #[test]
    fn an_empty_prefix_is_legal_and_an_empty_key_is_not() {
        assert!(validate_kv_prefix("").is_ok());
        assert!(validate_kv_key("").is_err());
        assert!(validate_kv_prefix("has space").is_err());
    }

    /// The scan's upper bound must exclude the prefix's own successors and
    /// nothing below them.
    #[test]
    fn the_prefix_end_is_the_next_string_above_the_range() {
        assert_eq!(prefix_end("okf:").as_deref(), Some("okf;"));
        assert_eq!(prefix_end("a").as_deref(), Some("b"));
        assert_eq!(prefix_end(""), None);

        // The property the SQL depends on, stated as a comparison rather than
        // as a literal: every key under the prefix sorts below the end.
        let end = prefix_end("okf:").unwrap();
        for k in ["okf:", "okf:a", "okf:zzzz", "okf::"] {
            assert!(k >= "okf:" && k < end.as_str(), "{k:?} outside the range");
        }
        for k in ["okf", "okg", "okf;a"] {
            assert!(!(k >= "okf:" && k < end.as_str()), "{k:?} inside the range");
        }
    }
}
