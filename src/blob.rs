//! `blobs`: content-addressed bytes, reclaimed by the archive (§4.10, [D-281],
//! [D-287], [D-288]).
//!
//! A blob is named by its SHA-256 digest, written as 64 lowercase hex
//! characters, and that text is the whole of the reference model: an
//! application stores the digest wherever it likes — a concept's `extra`, its
//! `content`, a link's `properties` — and the archive finds it there by
//! scanning the hot log's payloads for it ([D-287]). There is no refcount and
//! no `blob_gc`. A blob leaves the hot file only inside an archive session,
//! only when it was last put before the session's cutoff, and only when no hot
//! log entry names it; a blob a hot entry names that has already gone cold is
//! copied back ([D-288]).
//!
//! # What is not a reference
//!
//! Anything that does not contain the lowercase hex text: an uppercase digest,
//! a base64 one, a truncated prefix. That is one sentence on purpose. Every
//! narrower rule would add a way for the scan to miss a reference, and a missed
//! reference sends a blob cold while a hot entry still names it.
//!
//! # What carries it
//!
//! A snapshot is a `MaterializedState`: it carries digests inside `extra`, never
//! bytes. What carries blobs is a file-level backup of the hot file **and** the
//! cold file, the second because an archived blob lives there and only there.
//!
//! [D-281]: ../../docs/architecture/s13-decision-register.md#d-281
//! [D-287]: ../../docs/architecture/s13-decision-register.md#d-287
//! [D-288]: ../../docs/architecture/s13-decision-register.md#d-288

use std::collections::HashSet;

use crate::error::{DbError, Result};

/// The default for [`crate::Tuning::max_blob_bytes`]: 8 MiB ([D-281]).
///
/// libSQL 0.9.30 has no incremental blob I/O, so a put is one statement
/// carrying the whole value under the write lock, and a get materialises it in
/// one row. The cap keeps both of those a bounded cost; it is a `Tuning` field
/// rather than a constant because the right bound is the application's.
///
/// [D-281]: ../../docs/architecture/s13-decision-register.md#d-281
pub const DEFAULT_MAX_BLOB_BYTES: usize = 8 * 1024 * 1024;

/// Length of a digest as [`crate::Database::blob_put`] returns it.
pub const DIGEST_HEX_LEN: usize = 64;

/// Which file answered a [`crate::Database::blob_stat`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BlobLocation {
    /// The hot file's `blobs` table.
    Hot,
    /// The archive's `cold.blobs`: archived, still readable, and copied back
    /// by the next archive session if a hot log entry names it ([D-288]).
    ///
    /// [D-288]: ../../docs/architecture/s13-decision-register.md#d-288
    Cold,
}

/// A blob's metadata, without its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BlobStat {
    /// The address: 64 lowercase hex characters.
    pub sha256: String,
    /// Length of the bytes. Equal to them on disk by `CHECK` in the hot file.
    pub size: u64,
    /// When the blob was last put — or, for one copied back from the archive,
    /// when the session that restored it ran. The archive's age guard reads
    /// this ([D-281] amendment 1).
    ///
    /// [D-281]: ../../docs/architecture/s13-decision-register.md#d-281
    pub put_at: String,
    /// Which file answered.
    pub location: BlobLocation,
}

/// Check a digest: exactly 64 characters of `[0-9a-f]`.
///
/// Uppercase is refused rather than folded, for [`DbError::InvalidDigest`]'s
/// reason: it is not the address `blob_put` returned and the archive's scan
/// does not recognise it.
pub fn validate_digest(digest: &str) -> Result<()> {
    if digest.len() == DIGEST_HEX_LEN && digest.bytes().all(is_lower_hex) {
        Ok(())
    } else {
        Err(DbError::InvalidDigest(digest.to_string()))
    }
}

#[inline]
fn is_lower_hex(b: u8) -> bool {
    b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
}

/// The address of `bytes` ([D-281] amendment 3).
///
/// **The one place the address is decided**, and [`crate::Database::blob_put`]
/// is its only caller. Hex-encoded here rather than through a crate: the
/// encoding is part of the address, so it is spelled out where it can be read.
///
/// [D-281]: ../../docs/architecture/s13-decision-register.md#d-281
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(DIGEST_HEX_LEN);
    for b in digest.iter() {
        out.push(HEX[usize::from(b >> 4)] as char);
        out.push(HEX[usize::from(b & 0x0f)] as char);
    }
    out
}

/// Store `bytes` under `sha256`, or refresh `put_at` if the address is held.
///
/// Update first, insert on a miss: a re-put is the common case for an
/// application that puts on every save, and the `UPDATE` touches one narrow
/// index entry and never the bytes. The guard `trg_blobs_frozen_update` admits
/// it because `put_at` is the one column it does not name.
///
/// **A re-put must advance `put_at`**, and that is the age guard's whole input
/// ([D-281] amendment 1): a blob put before a cutoff and re-put after it is not
/// eligible, because the application has just said it wants it.
///
/// Returns whether a row was inserted.
///
/// [D-281]: ../../docs/architecture/s13-decision-register.md#d-281
pub(crate) async fn put(
    conn: &libsql::Connection,
    sha256: &str,
    bytes: &[u8],
    stamp: &str,
) -> Result<bool> {
    let refreshed = conn
        .execute(
            "UPDATE blobs SET put_at = ?1 WHERE sha256 = ?2",
            libsql::params![stamp, sha256],
        )
        .await?;
    if refreshed > 0 {
        return Ok(false);
    }
    conn.execute(
        "INSERT INTO blobs (sha256, size, bytes, put_at) VALUES (?1, ?2, ?3, ?4)",
        libsql::params![
            sha256,
            bytes.len() as i64,
            libsql::Value::Blob(bytes.to_vec()),
            stamp
        ],
    )
    .await?;
    Ok(true)
}

/// The bytes under `sha256` in `conn`'s `main.blobs`, or `None`.
///
/// Written against `main` so the same function serves the hot file and the
/// cold reader, which opens the archive as its own `main`.
pub(crate) async fn get(conn: &libsql::Connection, sha256: &str) -> Result<Option<Vec<u8>>> {
    let mut rows = conn
        .query(
            "SELECT bytes FROM blobs WHERE sha256 = ?1",
            libsql::params![sha256],
        )
        .await?;
    match rows.next().await? {
        Some(row) => match row.get_value(0)? {
            libsql::Value::Blob(b) => Ok(Some(b)),
            // The hot table's `CHECK (typeof(bytes) = 'blob')` makes this
            // unreachable there; a cold file carries no such check, and an
            // empty blob is the one value some writers store as text.
            libsql::Value::Text(t) => Ok(Some(t.into_bytes())),
            other => Err(DbError::Engine(libsql::Error::Misuse(format!(
                "blob {sha256} holds a {other:?}, not bytes"
            )))),
        },
        None => Ok(None),
    }
}

/// [`get`]'s metadata, without reading the bytes.
pub(crate) async fn stat(
    conn: &libsql::Connection,
    sha256: &str,
    location: BlobLocation,
) -> Result<Option<BlobStat>> {
    let mut rows = conn
        .query(
            "SELECT size, put_at FROM blobs WHERE sha256 = ?1",
            libsql::params![sha256],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(BlobStat {
            sha256: sha256.to_string(),
            size: row.get::<i64>(0)?.max(0) as u64,
            put_at: row.get(1)?,
            location,
        })),
        None => Ok(None),
    }
}

/// Whether `conn`'s `main` has a `blobs` table.
///
/// The cold reader asks before every lookup: a cold file written before 0.19
/// has none, and the reader must treat that as *absent* rather than create it
/// ([D-288]). Only the archive writer creates `cold.blobs`, inside its session.
///
/// [D-288]: ../../docs/architecture/s13-decision-register.md#d-288
pub(crate) async fn has_blobs_table(conn: &libsql::Connection) -> Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'blobs'",
            (),
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// The reference scan's state: which wanted digests one or more payloads named
/// ([D-287]).
///
/// # Windows, not tokens
///
/// Each maximal run of `[0-9a-f]` at least 64 long contributes every
/// 64-character window that is a member of `wanted`. A digest embedded in a
/// longer hex run is still the digest, and a token rule would miss it. Each
/// window is a set lookup, so memory is bounded by `|wanted|` rather than by
/// the log, and the scan stops reading as soon as every wanted digest has been
/// seen.
///
/// [D-287]: ../../docs/architecture/s13-decision-register.md#d-287
pub(crate) struct ReferenceScan<'a> {
    wanted: &'a HashSet<String>,
    found: HashSet<String>,
    /// Payload bytes fed so far — the figure [`crate::temporal::ArchiveReport`]
    /// reports, and the one gate 11 asserts is zero on a ledger with no blobs.
    pub(crate) bytes_scanned: u64,
}

impl<'a> ReferenceScan<'a> {
    pub(crate) fn new(wanted: &'a HashSet<String>) -> Self {
        Self {
            wanted,
            found: HashSet::new(),
            bytes_scanned: 0,
        }
    }

    /// Whether every wanted digest has been found, so reading further cannot
    /// change the answer.
    pub(crate) fn complete(&self) -> bool {
        self.found.len() == self.wanted.len()
    }

    /// Scan one payload.
    pub(crate) fn feed(&mut self, payload: &[u8]) {
        self.bytes_scanned += payload.len() as u64;
        let mut i = 0;
        while i < payload.len() {
            if !is_lower_hex(payload[i]) {
                i += 1;
                continue;
            }
            let start = i;
            while i < payload.len() && is_lower_hex(payload[i]) {
                i += 1;
            }
            if i - start >= DIGEST_HEX_LEN {
                self.windows(&payload[start..i]);
            }
        }
    }

    fn windows(&mut self, run: &[u8]) {
        for w in run.windows(DIGEST_HEX_LEN) {
            // The run is ASCII by construction, so this cannot fail.
            let Ok(s) = std::str::from_utf8(w) else {
                continue;
            };
            if self.wanted.contains(s) && !self.found.contains(s) {
                self.found.insert(s.to_string());
                if self.complete() {
                    return;
                }
            }
        }
    }

    /// The wanted digests the payloads named.
    pub(crate) fn into_found(self) -> HashSet<String> {
        self.found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FIPS 180-2 / NIST CSRC examples, the empty input included.
    #[test]
    fn the_address_is_sha256_in_lowercase_hex() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn digests_are_exactly_64_lowercase_hex() {
        let ok = sha256_hex(b"x");
        assert!(validate_digest(&ok).is_ok());
        for bad in [
            String::new(),
            ok[..63].to_string(),
            format!("{ok}0"),
            ok.to_uppercase(),
            format!("{}g", &ok[..63]),
        ] {
            assert!(
                matches!(validate_digest(&bad), Err(DbError::InvalidDigest(_))),
                "{bad:?} should be refused"
            );
        }
    }

    fn scan(wanted: &[&str], payloads: &[&str]) -> HashSet<String> {
        let wanted: HashSet<String> = wanted.iter().map(|s| s.to_string()).collect();
        let mut s = ReferenceScan::new(&wanted);
        for p in payloads {
            s.feed(p.as_bytes());
        }
        s.into_found()
    }

    #[test]
    fn a_digest_is_found_wherever_its_text_is() {
        let a = sha256_hex(b"a");
        let b = sha256_hex(b"b");
        let c = sha256_hex(b"c");
        let d = sha256_hex(b"d");
        let found = scan(
            &[&a, &b, &c, &d],
            &[
                &format!(r#"{{"extra":{{"att":"{a}"}}}}"#),
                &format!("https://example.test/blob/{b}?x=1"),
                // Embedded in a longer hex run, on both sides.
                &format!("0f{c}abc"),
            ],
        );
        assert!(found.contains(&a) && found.contains(&b) && found.contains(&c));
        assert!(!found.contains(&d));
    }

    #[test]
    fn uppercase_and_prefixes_are_not_references() {
        let a = sha256_hex(b"a");
        let found = scan(&[&a], &[&a.to_uppercase(), &a[..63]]);
        assert!(found.is_empty());
    }

    #[test]
    fn the_scan_counts_what_it_read() {
        let wanted = HashSet::new();
        let mut s = ReferenceScan::new(&wanted);
        s.feed(b"hello");
        assert_eq!(s.bytes_scanned, 5);
        assert!(s.complete());
    }
}
