use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use bincode::Options;

use crate::error::{DbError, Result};
use crate::temporal::replay::MaterializedState;
use crate::util::crc32::Crc32;

/// Header magic. Also the marker that separates a 0.5.5 snapshot from the
/// headerless files 0.5.4 and earlier wrote, whose first bytes are zstd's own
/// magic (`28 B5 2F FD`) and therefore never match this.
const SNAP_MAGIC: [u8; 4] = *b"MACR";

/// On-disk layout version for the snapshot container (D-043).
///
/// Bumped whenever the *shape* of [`MaterializedState`] changes, independently
/// of the database schema. `bincode` is not self-describing: adding a field
/// does not make an old file fail to parse, it makes it parse into the wrong
/// values — and a snapshot is the first thing a restart reaches for, so the
/// wrong values arrive labelled as the newest state anyone believed.
///
/// * **v2 (0.5.5)** adds the snapshot's own instant to the header (D-054).
/// * **v3 (0.13.12)** adds both lengths and a checksum (W8.2, D-185).
///
/// A v2 file meets a v3 build as [`DbError::SnapshotIncompatible`], which is
/// the case this versioned container was built for: the scan skips it and
/// folds from the log. No migration, because there is nothing to migrate —
/// a snapshot is a cache.
const SNAP_FORMAT_VERSION: u16 = 3;

/// The v3 container header, little-endian throughout:
///
/// ```text
/// offset  0      4    6      10               18            26           34     38
///         MACR | fmt | schema | taken_at_micros | payload_len | plain_len | crc32 |
///         (4)    (2)   (4)      (8)               (8)           (8)         (4)
/// ```
///
/// `payload_len` is the compressed byte count that follows this header,
/// `plain_len` what it decompresses to, and `crc32` covers the first 34 bytes
/// of the header **and** the payload — so the two lengths are themselves under
/// the checksum and a reader can trust them before acting on them (W8.2,
/// D-185).
const SNAP_HEADER_LEN: usize = 38;

/// Where the checksum sits: everything before it is covered by it.
const SNAP_CRC_OFFSET: usize = SNAP_HEADER_LEN - 4;

/// Microseconds since the Unix epoch, from the snapshot's own `timestamp`.
///
/// The instant is already in the payload — this is a *copy* in the header, which
/// is the kind of second description this codebase usually refuses. It earns the
/// exception by what reads it: retention has to bucket every snapshot by day, and
/// the alternative is decompressing and deserializing a full `MaterializedState`
/// per file on every pass, which would make the cadence's own maintenance cost
/// more than the work it exists to save. Eighteen bytes read without touching
/// zstd is the whole point of having a header at all (D-043).
///
/// It cannot drift from the payload because both are written from the same value
/// in the same statement, and nothing rewrites a snapshot in place.
fn taken_at_micros(state: &MaterializedState) -> u64 {
    crate::util::timestamp::parse(&state.timestamp)
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

/// Build the header for a payload, checksum included.
///
/// Takes the payload rather than a precomputed checksum so that there is one
/// place where the covered bytes are decided. A checksum passed in as a `u32`
/// would let a caller compute it over the wrong range, and the failure mode of
/// that is a file that verifies against itself and nothing else.
fn snapshot_header(
    schema_version: u32,
    taken_at: u64,
    payload: &[u8],
    plain_len: u64,
) -> [u8; SNAP_HEADER_LEN] {
    let mut h = [0u8; SNAP_HEADER_LEN];
    h[0..4].copy_from_slice(&SNAP_MAGIC);
    h[4..6].copy_from_slice(&SNAP_FORMAT_VERSION.to_le_bytes());
    h[6..10].copy_from_slice(&schema_version.to_le_bytes());
    h[10..18].copy_from_slice(&taken_at.to_le_bytes());
    h[18..26].copy_from_slice(&(payload.len() as u64).to_le_bytes());
    h[26..34].copy_from_slice(&plain_len.to_le_bytes());

    let mut crc = Crc32::new();
    crc.update(&h[..SNAP_CRC_OFFSET]);
    crc.update(payload);
    h[SNAP_CRC_OFFSET..].copy_from_slice(&crc.finish().to_le_bytes());
    h
}

/// The instant a snapshot reflects, read from its header alone.
///
/// `None` for anything this build would refuse to load anyway — a foreign file,
/// an older container, a truncated one. Retention treats that as "no date" and
/// falls back to the newest-N rule for it rather than guessing.
fn header_taken_at(path: &Path) -> Option<u64> {
    let mut file = fs::File::open(path).ok()?;
    let mut head = [0u8; SNAP_HEADER_LEN];
    file.read_exact(&mut head).ok()?;
    if head[0..4] != SNAP_MAGIC {
        return None;
    }
    if u16::from_le_bytes([head[4], head[5]]) != SNAP_FORMAT_VERSION {
        return None;
    }
    let micros = u64::from_le_bytes(head[10..18].try_into().ok()?);
    (micros > 0).then_some(micros)
}

/// Zero-padding width for the `seq_id` in a snapshot filename.
///
/// `seq_id` is an `INTEGER PRIMARY KEY AUTOINCREMENT`, so its ceiling is
/// `i64::MAX` — 19 digits. The previous `{:08}` produced names that stopped
/// sorting in `seq_id` order the moment the ledger passed 10^8 entries, which is
/// the same fixed-width failure D-029 describes, deferred rather than avoided.
/// Retention no longer *depends* on this (see [`cleanup_expired_snapshots`]),
/// but a directory listing should still read in order.
const SEQ_WIDTH: usize = 19;

/// The snapshot file for a given anchor.
fn snapshot_filename(seq_anchor: i64) -> String {
    format!("{seq_anchor:0SEQ_WIDTH$}.snap.zst")
}

/// Recover the anchor a snapshot filename encodes.
pub(crate) fn seq_from_filename(path: &Path) -> Option<i64> {
    path.file_name()?
        .to_str()?
        .strip_suffix(".snap.zst")?
        .parse()
        .ok()
}

/// Make the *directory entry* durable, on the platforms that have a way to say
/// so (0.13.13, W8.3,
/// [D-186](../../docs/architecture/s13-decision-register.md#d-186)).
///
/// `fs::rename` is atomic, and atomic is not durable. The rename decides
/// *which* file is at the final name — a crash across it leaves the old
/// snapshot or the new one, never a splice — but the name itself lives in the
/// directory, and a directory's own metadata reaches the disk when the
/// filesystem feels like it. The window is a real one and its shape is
/// unhelpful: the file's bytes are already `fsync`ed, so what a power loss
/// takes is the *pointer*, leaving a perfectly good snapshot under a name
/// nothing looks for while the newest name still resolves to an older file.
///
/// This is the standard POSIX gap, and the crash it matters on is precisely the
/// crash a snapshot exists for.
#[cfg(unix)]
fn sync_directory(dir: &Path) -> std::io::Result<()> {
    // Read-only is enough and is also all that is on offer: `fsync` on a
    // directory descriptor flushes that directory's metadata, and a directory
    // cannot be opened for writing.
    fs::File::open(dir)?.sync_all()
}

/// Windows and anything else: nothing, deliberately and by name (0.13.13, W8.3,
/// [D-186](../../docs/architecture/s13-decision-register.md#d-186)).
///
/// There is no directory `fsync` on Windows. A directory *handle* can be opened
/// with `FILE_FLAG_BACKUP_SEMANTICS`, but `FlushFileBuffers` needs write access
/// on the handle and a directory does not grant it; the call that does cover
/// directory metadata takes a volume handle, requires administrative
/// privileges, and flushes every open file on the volume — which is not a thing
/// a library may do to its host process's machine.
///
/// What stands in for it is NTFS's own metadata journal: the rename is a
/// logged transaction, so a completed rename is recovered by the filesystem
/// rather than by anything this crate arranges. That is a genuinely weaker
/// statement than the `unix` branch makes — it rests on the filesystem being
/// NTFS or ReFS, and says nothing about FAT32 or a network share — and it is
/// written down rather than assumed, because a silent no-op is how a durability
/// gap survives being closed.
#[cfg(not(unix))]
fn sync_directory(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Save a bincode-serialized, zstd-compressed snapshot file (.snap.zst) (§5.5).
///
/// Written to a temporary file, flushed to disk, renamed into place, and the
/// directory flushed after the rename. A snapshot is read back with no
/// integrity check beyond the container's own checksum, so a half-written file
/// at the final name is a file that looks loadable and is not — and it would be
/// the *newest* one, which is exactly the one a restart reaches for. Rename
/// within a directory is atomic, so a crash leaves either the old snapshot or
/// the new one, never a splice; `sync_directory` is what makes the winner of
/// that race survive the power loss that caused it (0.13.13, W8.3).
///
/// # This blocks, and it is not a small block (0.13.11, W8.1)
///
/// bincode over the whole state, zstd over the result, a file write and an
/// `fsync` — CPU and disk, both unbounded in the size of the graph, and none of
/// it yielding. Called from an async task it stalls that runtime worker for the
/// whole duration, which at 100K edges is the two seconds §9 budgets for it.
/// Every async caller inside the crate goes through `save_and_prune`; a
/// caller outside it wants `tokio::task::spawn_blocking` around this, and the
/// signature stays synchronous so that they can have it.
pub fn save_snapshot(snapshots_dir: &Path, state: &MaterializedState) -> Result<PathBuf> {
    let fail = |what: &str, e: std::io::Error| DbError::ReplayCorrupt {
        seq: state.seq_anchor,
        reason: format!("{what}: {e}"),
    };

    fs::create_dir_all(snapshots_dir)
        .map_err(|e| fail("failed to create snapshot directory", e))?;

    let path = snapshots_dir.join(snapshot_filename(state.seq_anchor));
    let tmp_path = path.with_extension("tmp");

    let serialized = bincode::serialize(state).map_err(|e| DbError::ReplayCorrupt {
        seq: state.seq_anchor,
        reason: format!("failed to serialize snapshot: {e}"),
    })?;

    let compressed =
        zstd::encode_all(&serialized[..], 3).map_err(|e| fail("failed to compress snapshot", e))?;

    let mut file =
        fs::File::create(&tmp_path).map_err(|e| fail("failed to create snapshot temp file", e))?;
    // Header first, uncompressed: it has to be readable without committing to
    // decompressing a payload this build may not understand (D-043). Since v3
    // it also carries the checksum over the payload that follows it, which is
    // why it is built after the compression rather than before (W8.2, D-185).
    file.write_all(&snapshot_header(
        crate::schema::migrations::SCHEMA_VERSION,
        taken_at_micros(state),
        &compressed,
        serialized.len() as u64,
    ))
    .map_err(|e| fail("failed to write snapshot header", e))?;
    file.write_all(&compressed)
        .map_err(|e| fail("failed to write snapshot bytes", e))?;
    // Before the rename, or the rename can land ahead of the data.
    file.sync_all()
        .map_err(|e| fail("failed to flush snapshot to disk", e))?;
    drop(file);

    fs::rename(&tmp_path, &path).map_err(|e| {
        let _ = fs::remove_file(&tmp_path);
        fail("failed to publish snapshot", e)
    })?;

    // After the rename, because it is the rename that has to survive. The file
    // is already at its final name when this runs, so a failure here does not
    // mean the snapshot is missing or damaged — it means this function cannot
    // promise the name outlives a power loss, which is the whole of what it
    // promises past `sync_all` above, and so it is reported rather than logged
    // (W8.3, D-186).
    sync_directory(snapshots_dir)
        .map_err(|e| fail("failed to make the snapshot's directory entry durable", e))?;

    Ok(path)
}

/// Load a snapshot, refusing anything this build cannot read (§5.5, D-043).
///
/// The header is checked *before* the payload is decompressed, and a mismatch
/// is [`DbError::SnapshotIncompatible`] rather than a corruption error, because
/// the two want opposite responses: corruption is a fault to report, an
/// incompatible snapshot is an ordinary consequence of upgrading and the right
/// answer is to discard it and cold-fold. Distinguishing them is the whole
/// point of the header — `bincode` is not self-describing, so without one an
/// old file does not reliably fail to parse, it parses into wrong values.
///
/// Headerless files written by 0.5.4 and earlier are rejected by the same path:
/// their first four bytes are zstd's magic, which is not `MACR`.
///
/// # Damage is a third answer, and it is bounded (0.13.12, W8.2, D-185)
///
/// [`DbError::SnapshotCorrupt`] is not [`DbError::SnapshotIncompatible`] and
/// not [`DbError::ReplayCorrupt`]: the file is damaged, the ledger is not, and
/// the repair is to delete the file. Every failure below used to be
/// `ReplayCorrupt { seq: 0 }`, which said the log was damaged and carried a
/// sequence number that cannot exist.
///
/// The checks run in the order that lets the cheapest one fire first, and each
/// is a named error rather than a symptom further down:
///
/// 1. **Declared payload length** against the bytes actually present. Catches
///    truncation and trailing junk without hashing anything.
/// 2. **Checksum** over the header and the payload, before zstd is handed a
///    single byte. This is the check that closes §3.3: a corrupt stream is
///    refused *as* a corrupt stream, rather than being walked to exhaustion by
///    a deserializer trying to make sense of it.
/// 3. **Declared plaintext length**, enforced during decompression rather than
///    checked after it — the reader is bounded to `plain_len + 1` bytes, so a
///    frame that expands further stops at the bound instead of allocating.
/// 4. **A bincode limit** equal to the buffer's own size, replacing the
///    `Infinite` limit `bincode::deserialize` carries.
///
/// Steps 3 and 4 are redundant with step 2 for every file this crate wrote,
/// and that is the point of having them: they hold when the checksum has
/// already been satisfied by something that computed it deliberately.
///
/// # This blocks (0.13.11, W8.1)
///
/// Read, decompress, deserialize, all synchronous — see [`save_snapshot`] for
/// the argument. The crate's one async reader is `snapshot_anchor`, which
/// offloads the whole scan rather than each file.
pub fn load_snapshot(path: &Path) -> Result<MaterializedState> {
    let label = path.display().to_string();
    let raw = fs::read(path).map_err(|e| DbError::SnapshotCorrupt {
        path: label.clone(),
        reason: format!("could not be read: {e}"),
    })?;
    parse_snapshot(&label, &raw)
}

/// The half of [`load_snapshot`] that is a parser (0.13.14, W8.4, D-187).
///
/// Split out because a parser that can only be reached through a filesystem
/// path is a parser that can only be fuzzed through the filesystem: a syscall
/// round trip per case, on the one axis where cases per second is the entire
/// measure of the tool. `load_snapshot` is now *read the file* and this is
/// *understand the bytes*, which is also the honest description of what the
/// two halves were already doing.
///
/// `label` is what the error carries as `path`. It is a `&str` rather than a
/// `&Path` because the caller that is not a file — the fuzz harness — does not
/// have one, and inventing a fake path so the signature could keep its type
/// would be putting a lie in every error message it produced.
pub(crate) fn parse_snapshot(label: &str, raw: &[u8]) -> Result<MaterializedState> {
    let damaged = |reason: String| DbError::SnapshotCorrupt {
        path: label.to_string(),
        reason,
    };
    let foreign = |reason: String| DbError::SnapshotIncompatible {
        path: label.to_string(),
        reason,
    };

    if raw.len() < SNAP_HEADER_LEN || raw[0..4] != SNAP_MAGIC {
        return Err(foreign(
            "not a macrame snapshot, or written before the versioned container \
             existed (0.5.4 and earlier)"
                .to_string(),
        ));
    }

    let format = u16::from_le_bytes([raw[4], raw[5]]);
    let schema = u32::from_le_bytes([raw[6], raw[7], raw[8], raw[9]]);
    let expected_schema = crate::schema::migrations::SCHEMA_VERSION;
    if format != SNAP_FORMAT_VERSION || schema != expected_schema {
        return Err(foreign(format!(
            "snapshot is format v{format}/schema v{schema}; this build reads \
             format v{SNAP_FORMAT_VERSION}/schema v{expected_schema}"
        )));
    }

    // Unwraps: the slices are fixed ranges of a buffer already checked to be at
    // least SNAP_HEADER_LEN long, so `try_into` on each cannot fail.
    let payload_len = u64::from_le_bytes(raw[18..26].try_into().unwrap());
    let plain_len = u64::from_le_bytes(raw[26..34].try_into().unwrap());
    let declared_crc =
        u32::from_le_bytes(raw[SNAP_CRC_OFFSET..SNAP_HEADER_LEN].try_into().unwrap());

    let payload = &raw[SNAP_HEADER_LEN..];
    if payload.len() as u64 != payload_len {
        return Err(damaged(format!(
            "the header declares {payload_len} payload bytes and the file \
             carries {}: truncated, or something was appended",
            payload.len()
        )));
    }

    let mut crc = Crc32::new();
    crc.update(&raw[..SNAP_CRC_OFFSET]);
    crc.update(payload);
    let actual_crc = crc.finish();
    if actual_crc != declared_crc {
        return Err(damaged(format!(
            "checksum mismatch: the header declares {declared_crc:#010x} and \
             the bytes hash to {actual_crc:#010x}"
        )));
    }

    // Bounded at `plain_len + 1` so that a frame claiming to be larger than it
    // said stops one byte over the line rather than at whatever it decides to
    // expand to. `saturating_add` because `plain_len` is a number off a disk.
    let mut decoder =
        zstd::Decoder::new(payload).map_err(|e| damaged(format!("zstd rejected it: {e}")))?;
    let mut plain = Vec::new();
    decoder
        .by_ref()
        .take(plain_len.saturating_add(1))
        .read_to_end(&mut plain)
        .map_err(|e| damaged(format!("could not be decompressed: {e}")))?;
    if plain.len() as u64 != plain_len {
        return Err(damaged(format!(
            "the header declares {plain_len} plaintext bytes and the payload \
             decompressed to {}",
            plain.len()
        )));
    }

    // `bincode::deserialize`'s own options, plus a limit: the default is
    // `Infinite`, and the buffer's length is the only honest bound available
    // once the bytes are in hand.
    let state: MaterializedState = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .allow_trailing_bytes()
        .with_limit(plain.len() as u64)
        .deserialize(&plain)
        .map_err(|e| damaged(format!("could not be deserialized: {e}")))?;

    Ok(state)
}

/// [`save_snapshot`] then [`cleanup_expired_snapshots`], on a blocking thread
/// (0.13.11, W8.1, D-184).
///
/// The whole write side of the snapshot in one hop, because the two run back to
/// back and a second `spawn_blocking` between them would buy a scheduling point
/// nobody is waiting at. Order and error behaviour are exactly what they were
/// inline: a failed save is returned and the prune does not run, a failed prune
/// is returned even though the snapshot is already on disk.
///
/// # Losing the thread is an error and not a shrug
///
/// A `spawn_blocking` task cannot be cancelled once it has started, so the only
/// way `await` yields a [`tokio::task::JoinError`] here is that the closure
/// panicked. That closure is the code that writes the file `close()` promises
/// to have written, so the panic becomes [`DbError::ReplayCorrupt`] carrying
/// the anchor — the same class every other failure of `save_snapshot` reports,
/// which is the point: a caller handling "the snapshot did not get written"
/// should not need a second arm for the case where it failed by panicking.
///
/// This is the opposite call from the read side, where a failed load costs
/// speed and nothing else — see `snapshot_anchor`.
async fn save_and_prune(snapshots_dir: PathBuf, state: MaterializedState) -> Result<PathBuf> {
    let seq = state.seq_anchor;
    tokio::task::spawn_blocking(move || {
        let path = save_snapshot(&snapshots_dir, &state)?;
        cleanup_expired_snapshots(&snapshots_dir)?;
        Ok(path)
    })
    .await
    .unwrap_or_else(|e| {
        Err(DbError::ReplayCorrupt {
            seq,
            reason: format!("the thread writing the snapshot did not finish: {e}"),
        })
    })
}

/// Write the final snapshot on clean shutdown (§5.1.7).
///
/// Called after the Write Actor has stopped, so the state it folds is quiescent
/// — nothing can commit between the fold and the write. Returns the snapshot's
/// path so a caller can log or verify it.
///
/// This was a `Ok(())` stub that `close()` never called, which meant every
/// restart replayed the log from whatever snapshot happened to be lying around
/// rather than from the shutdown anchor.
///
/// The fold is async and the write is not, so the write goes to a blocking
/// thread (`save_and_prune`, 0.13.11, W8.1). Both halves used to run on the
/// caller's worker, and the second half is the expensive one.
pub async fn write_final(
    conn: &libsql::Connection,
    snapshots_dir: &Path,
    ts: &str,
    archive_path: Option<&Path>,
) -> Result<PathBuf> {
    let state =
        crate::temporal::replay::reconstruct(conn, ts, archive_path, Some(snapshots_dir)).await?;
    save_and_prune(snapshots_dir.to_path_buf(), state).await
}

/// Snapshots kept unconditionally, newest first, by [`cleanup_expired_snapshots`] (§5.5).
const RETAIN: usize = 5;

/// Days for which one snapshot each is kept beyond [`RETAIN`] (§5.5, D-054).
const RETAIN_DAYS: i64 = 30;

const MICROS_PER_DAY: u64 = 86_400_000_000;

/// Retention: the newest `RETAIN`, **plus one per day for `RETAIN_DAYS`**
/// (§5.5, D-054).
///
/// **Why the daily tier exists, and why it did not matter until now.** Through
/// 0.5.4 a snapshot was written once per clean shutdown, so "newest five" was
/// five shutdowns — days or weeks of coverage, and the daily rule §5.5 specifies
/// bought nothing. The cadence ([D-053](../../docs/architecture/s13-decision-register.md))
/// writes one every 10,000 log entries, so under load five anchors can span
/// minutes: every instant older than that falls back to folding the whole log,
/// which is the cost snapshots exist to avoid. The flat rule went from harmless
/// to actively defeating the feature that had just been added.
///
/// Ordered by the `seq_id` parsed out of each filename, not by the filename
/// itself. A lexicographic sort over names is only `seq_id` order while every
/// name is the same width, and "delete the oldest" reading from a mis-sorted
/// list deletes the wrong files — quietly, and preferentially the newest ones.
/// Parsing removes the dependency on `SEQ_WIDTH` entirely.
///
/// A snapshot whose header carries no readable instant survives only under the
/// newest-`RETAIN` rule. That is deliberate: it is a file this build would
/// refuse to *load* anyway, so keeping it for its date would be keeping it for a
/// date nothing will ever use.
pub fn cleanup_expired_snapshots(snapshots_dir: &Path) -> Result<usize> {
    if !snapshots_dir.exists() {
        return Ok(0);
    }

    let read_dir = fs::read_dir(snapshots_dir).map_err(|e| DbError::ReplayCorrupt {
        seq: 0,
        reason: format!("failed to read snapshot dir: {e}"),
    })?;

    // (seq_id, path, day since epoch — None when the header carries no instant)
    let mut snapshots: Vec<(i64, PathBuf, Option<i64>)> = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        match path.extension().and_then(|e| e.to_str()) {
            // A leftover from an interrupted save. It was never renamed into
            // place, so nothing can be reading it, and left alone these
            // accumulate forever.
            Some("tmp") => {
                let _ = fs::remove_file(&path);
            }
            Some("zst") => match seq_from_filename(&path) {
                Some(seq) => {
                    let day = header_taken_at(&path).map(|micros| (micros / MICROS_PER_DAY) as i64);
                    snapshots.push((seq, path, day));
                }
                // Not ours, or a name we cannot order. Deleting on a guess is
                // how retention turns into data loss.
                None => tracing::warn!("snapshot cleanup: unparseable filename {path:?}, skipping"),
            },
            _ => {}
        }
    }

    snapshots.sort_by_key(|(seq, _, _)| *seq);

    let mut keep: std::collections::HashSet<&PathBuf> = snapshots
        .iter()
        .rev()
        .take(RETAIN)
        .map(|(_, path, _)| path)
        .collect();

    // One per day, for the last RETAIN_DAYS days. "Today" is the newest
    // snapshot's own day rather than the wall clock: retention is then a
    // function of the directory's contents and nothing else, so it is
    // deterministic and testable — and a database left untouched for a year does
    // not have its entire history deleted by the first write after it wakes up.
    if let Some(today) = snapshots.iter().filter_map(|(_, _, day)| *day).max() {
        let horizon = today - (RETAIN_DAYS - 1);
        let mut newest_of_day: std::collections::BTreeMap<i64, &PathBuf> =
            std::collections::BTreeMap::new();
        // Ascending by seq, so the last write for a day wins its slot.
        for (_, path, day) in &snapshots {
            if let Some(day) = *day {
                if day >= horizon {
                    newest_of_day.insert(day, path);
                }
            }
        }
        keep.extend(newest_of_day.into_values());
    }

    let doomed: Vec<PathBuf> = snapshots
        .iter()
        .filter(|(_, path, _)| !keep.contains(path))
        .map(|(_, path, _)| path.clone())
        .collect();

    // No directory sync after these (W8.3, D-186), and the asymmetry is the
    // point: a deletion that a crash undoes resurrects a *valid* snapshot,
    // which the next pass deletes again. A creation that a crash undoes loses
    // the anchor. Durability is owed to the name that has to be there, not to
    // the name that has to be gone.
    let mut removed = 0;
    for path in doomed {
        if let Err(e) = fs::remove_file(&path) {
            tracing::warn!("failed to remove expired snapshot {path:?}: {e}");
        } else {
            removed += 1;
        }
    }

    Ok(removed)
}

// ---------------------------------------------------------------------------
// The maintenance cadence (§5.5, D-053)
// ---------------------------------------------------------------------------

/// How often the maintenance task writes an anchor (§5.5).
///
/// §5.5 specifies "every 10,000 log entries", which is a *distance* rather than
/// a schedule — the point is to bound how much delta a reconstruction has to
/// fold, and delta is measured in log entries, not seconds. An idle database
/// therefore writes nothing at all, however long it stays open.
///
/// `poll_interval` is how often that distance is checked, and it is the part
/// §5.5 does not specify because it is an implementation cost rather than a
/// property: the check is `SELECT MAX(seq_id)`, an index lookup on an integer
/// primary key, so the interval trades a negligible read against how promptly a
/// burst of writes is noticed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotCadence {
    /// Write an anchor once the log has grown this many entries past the last.
    pub every_entries: i64,
    /// How often to compare the log's head against the last anchor.
    pub poll_interval: std::time::Duration,
}

impl Default for SnapshotCadence {
    fn default() -> Self {
        Self {
            every_entries: 10_000,
            poll_interval: std::time::Duration::from_secs(5),
        }
    }
}

/// The newest anchor already on disk, as a `seq_id`, or 0 if there is none.
///
/// Read from the filenames rather than remembered across runs: a process that
/// starts against a database someone else has been writing should not re-anchor
/// immediately, and the files are the only record of what has been anchored.
///
/// Left on the caller's worker where [`save_and_prune`] was moved off it
/// (0.13.11, W8.1): one `read_dir` over a directory retention holds to about
/// `RETAIN + RETAIN_DAYS` entries, no file opened and nothing decompressed,
/// run once when the cadence starts. `spawn_blocking` is not free, and paying
/// it to move a bounded directory listing would be cargo-culting the fix.
fn newest_anchor_on_disk(snapshots_dir: &Path) -> i64 {
    let Ok(entries) = fs::read_dir(snapshots_dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter_map(|p| seq_from_filename(&p))
        .max()
        .unwrap_or(0)
}

async fn log_head(conn: &libsql::Connection) -> Result<Option<(i64, String)>> {
    let mut rows = conn
        .query(
            "SELECT MAX(seq_id), MAX(recorded_at) FROM transaction_log",
            (),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    match (row.get::<i64>(0), row.get::<String>(1)) {
        (Ok(seq), Ok(ts)) => Ok(Some((seq, ts))),
        // An empty log yields one row of NULLs, not zero rows.
        _ => Ok(None),
    }
}

/// The read-side maintenance task §5.5 specifies (D-053).
///
/// Everything it does is a read plus a file write, so it never touches the write
/// connection and cannot lengthen the actor's loop — which is the whole reason
/// §5.5 puts snapshotting on the read side, since §5.1.5's latency bound is a
/// property of how long that loop can take.
///
/// It anchors at `MAX(recorded_at)` rather than at the clock's `now()`. The two
/// differ by however long it has been since the last write, and anchoring at a
/// timestamp *after* the newest entry would produce a snapshot whose contents
/// are identical but whose name and header claim a later instant than anything
/// it reflects. Anchoring at the newest belief keeps the file honest about what
/// it is a snapshot *of*.
///
/// Failures are logged and retried on the next tick rather than ending the task.
/// A snapshot is a cache: failing to write one costs a slower reconstruction and
/// nothing else, and a maintenance task that exits on its first transient error
/// is indistinguishable from one that was never spawned.
pub(crate) async fn run_cadence(
    conn: libsql::Connection,
    snapshots_dir: PathBuf,
    archive_path: PathBuf,
    cadence: SnapshotCadence,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let mut anchored = newest_anchor_on_disk(&snapshots_dir);

    loop {
        tokio::select! {
            biased;
            // Dropped sender counts as a stop, so a `Database` that is dropped
            // rather than closed does not leave this running against a
            // connection whose database is going away.
            _ = stop.changed() => return,
            _ = tokio::time::sleep(cadence.poll_interval) => {}
        }

        let head = match log_head(&conn).await {
            Ok(Some(head)) => head,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!("snapshot cadence: could not read the log head: {e}");
                continue;
            }
        };
        let (max_seq, ts) = head;

        if max_seq - anchored < cadence.every_entries {
            continue;
        }

        let archive = archive_path.exists().then_some(archive_path.as_path());
        match write_final(&conn, &snapshots_dir, &ts, archive).await {
            Ok(path) => {
                anchored = seq_from_filename(&path).unwrap_or(max_seq);
                tracing::debug!("snapshot cadence: anchored at seq {anchored} ({path:?})");
            }
            Err(e) => {
                // Deliberately does not advance `anchored`: the next tick
                // retries rather than waiting another whole interval's worth of
                // entries after a failure.
                tracing::warn!("snapshot cadence: failed to write an anchor: {e}");
            }
        }
    }
}

/// Wrap arbitrary plaintext in a container that passes every check the checksum
/// guards (0.13.14, W8.4,
/// [D-187](../../docs/architecture/s13-decision-register.md#d-187)).
///
/// **A checksummed format is fuzz-hostile, and this is the answer to that.**
/// Coverage-guided mutation finds a four-byte magic quickly; it does not find a
/// CRC-32 that has to agree with 34 header bytes *and* the whole payload. A
/// fuzzer pointed at the container as a whole therefore spends its budget being
/// turned away at step two and never reaches zstd or bincode — the two
/// components W8.2 bounded, and the two where a real defect would live. This
/// builds the container the way [`save_snapshot`] builds it, around whatever
/// bytes it is handed, which puts every input past the gate.
///
/// It is the same move the W8.2 unit tests make when they forge a *valid*
/// checksum on purpose: what is under test is the reader once integrity has
/// been satisfied by something that computed it deliberately, because that is
/// the case the bounds after the checksum exist for.
#[cfg(any(test, feature = "fuzzing"))]
pub(crate) fn wrap_plaintext(plain: &[u8]) -> Vec<u8> {
    // Level 3, as `save_snapshot` uses. A caller mutating the plaintext is
    // mutating what the deserializer sees, which is the point; the compression
    // in between is not what is being explored.
    let compressed = zstd::encode_all(plain, 3).expect("in-memory zstd encode");
    wrap_payload(&compressed, plain.len() as u64)
}

/// [`wrap_plaintext`] one layer lower: a valid container around bytes that do
/// not have to be a zstd frame, under a plaintext length that does not have to
/// be true (0.13.14, W8.4, D-187).
///
/// This is the shape that reaches step 3 of the reader — decompression bounded
/// by a *declared* length — with the checksum already satisfied. It is how a
/// decompression bomb is expressed: a frame that expands to far more than the
/// header admits to, signed correctly, which is the case
/// [D-185](../../docs/architecture/s13-decision-register.md#d-185) argues the
/// bound must survive because the checksum cannot help with it.
#[cfg(any(test, feature = "fuzzing"))]
pub(crate) fn wrap_payload(payload: &[u8], plain_len: u64) -> Vec<u8> {
    let header = snapshot_header(
        crate::schema::migrations::SCHEMA_VERSION,
        0,
        payload,
        plain_len,
    );
    let mut out = Vec::with_capacity(header.len() + payload.len());
    out.extend_from_slice(&header);
    out.extend_from_slice(payload);
    out
}

// ---------------------------------------------------------------------------
// Doors for `fuzz/`, and for nothing else (0.13.14, W8.4, D-187)
// ---------------------------------------------------------------------------

/// Reachable only with `--features fuzzing`, which nothing but `fuzz/` turns on
/// (0.13.14, W8.4,
/// [D-187](../../docs/architecture/s13-decision-register.md#d-187)).
///
/// `#[doc(hidden)]` and feature-gated rather than public: these are not an API,
/// they are the two places a fuzz harness has to reach that a caller has no
/// business reaching. The default build does not compile this module at all, so
/// the crate's public surface is unchanged by its existence.
#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub mod fuzzing {
    use super::*;

    /// Exactly what [`load_snapshot`] does once it has the bytes.
    ///
    /// The fuzz target for the container as a whole. Every input is a candidate
    /// file; the property is that it comes back as a state or as a **named**
    /// error, and never as a panic.
    pub fn parse(raw: &[u8]) -> Result<MaterializedState> {
        parse_snapshot("<fuzz>", raw)
    }

    /// `super::wrap_plaintext`, which the in-suite mutation tests also use —
    /// one construction, so the fuzzer and the deterministic tests are
    /// exercising the same container and not two descriptions of one.
    ///
    /// The input is the **plaintext**, so what a fuzzer explores through this
    /// door is `bincode`'s decoder: zstd always sees a frame this function just
    /// produced.
    pub fn wrap_plaintext(plain: &[u8]) -> Vec<u8> {
        super::wrap_plaintext(plain)
    }

    /// `super::wrap_payload`: a correct container around bytes that need not
    /// be a zstd frame, under a plaintext length that need not be true.
    ///
    /// The door for the layer between the other two. What a fuzzer explores
    /// through this one is **zstd** and the declared-length bound — including
    /// the decompression bomb, which is the one input in this format whose
    /// checksum can be perfectly correct and whose reader still has to refuse
    /// it.
    pub fn wrap_payload(payload: &[u8], plain_len: u64) -> Vec<u8> {
        super::wrap_payload(payload, plain_len)
    }

    /// Take a real snapshot apart, so a corpus for the two inner targets can be
    /// derived from a file `save_snapshot` actually wrote.
    ///
    /// This exists so that seeds are never *transcribed*. A seed generator that
    /// built its own plaintext would be a second description of what the writer
    /// produces, drifting the first time either end changes — and a corpus that
    /// has drifted still looks like a corpus, so nothing would say so. Reading
    /// the payload out of a genuine container cannot be wrong about the format
    /// while the format is what this build writes.
    ///
    /// `None` for anything that is not a container this build recognises.
    pub fn payload_of(container: &[u8]) -> Option<(&[u8], u64)> {
        if container.len() < SNAP_HEADER_LEN || container[0..4] != SNAP_MAGIC {
            return None;
        }
        let plain_len = u64::from_le_bytes(container[26..34].try_into().ok()?);
        Some((&container[SNAP_HEADER_LEN..], plain_len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::temporal::as_of::NodeAttributes;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    const TS: &str = "2026-08-24T12:00:00.000000Z";

    /// A state big enough that serializing and compressing it is measurable
    /// work rather than a few microseconds.
    ///
    /// The size is the test: a state small enough to compress instantly would
    /// pass whether the work was offloaded or not, because the current-thread
    /// executor would never get a turn either way.
    fn bulky_state(seq: i64) -> MaterializedState {
        let mut concepts = HashMap::new();
        for i in 0..20_000u32 {
            concepts.insert(
                format!("c{i}"),
                NodeAttributes {
                    id: format!("c{i}"),
                    title: format!("concept number {i}"),
                    content: format!("{i} ").repeat(40),
                    embedding_model: None,
                },
            );
        }
        MaterializedState {
            seq_anchor: seq,
            timestamp: TS.to_string(),
            concepts,
            edges: Vec::new(),
            predates_recorded_history: false,
        }
    }

    /// §2.4, and the property W8.1 exists for.
    ///
    /// On a **current-thread** runtime there is exactly one worker, so "does
    /// this block the runtime" stops being a question about load and becomes a
    /// question about whether any other task runs at all. Inline, the whole
    /// serialize-compress-write-fsync sequence sits between two scheduling
    /// points and the ticker gets zero turns; offloaded, awaiting the join
    /// handle yields and the ticker runs for the duration.
    ///
    /// The single-worker runtime is also what makes this a regression test
    /// against the wrong fix: `block_in_place` would move the work off the
    /// *async* path but panics outside a multi-threaded runtime, so a rewrite
    /// that reached for it would fail here rather than in production.
    #[test]
    fn the_snapshot_write_does_not_hold_the_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let ticks = rt.block_on(async {
            let ticks = Arc::new(AtomicU64::new(0));
            let counter = Arc::clone(&ticks);
            let ticker = tokio::spawn(async move {
                loop {
                    counter.fetch_add(1, Ordering::Relaxed);
                    tokio::task::yield_now().await;
                }
            });

            save_and_prune(dir.path().to_path_buf(), bulky_state(1))
                .await
                .expect("the snapshot must still be written");

            ticker.abort();
            ticks.load(Ordering::Relaxed)
        });

        assert!(
            ticks > 0,
            "no other task ran while the snapshot was being written: the \
             serialisation is back on the runtime worker (§2.4, W8.1)"
        );
    }

    /// Moving the work to another thread must not change what lands on disk.
    ///
    /// The offload is a scheduling change and nothing else, so the file it
    /// produces has to be the file [`save_snapshot`] produced when the same
    /// call ran inline — same name, same header, same state coming back out.
    #[test]
    fn a_snapshot_written_off_thread_reads_back_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let state = bulky_state(77);
        let path = rt
            .block_on(save_and_prune(dir.path().to_path_buf(), state.clone()))
            .unwrap();

        assert_eq!(seq_from_filename(&path), Some(77));
        let loaded = load_snapshot(&path).unwrap();
        assert_eq!(loaded.seq_anchor, state.seq_anchor);
        assert_eq!(loaded.timestamp, state.timestamp);
        assert_eq!(loaded.concepts.len(), state.concepts.len());
        assert_eq!(loaded.concepts["c19999"], state.concepts["c19999"]);
    }

    /// Rewrite a saved snapshot's header with a doctored `plain_len`, checksum
    /// and all.
    ///
    /// The checksum is *recomputed*, which is the point: these tests are about
    /// what the reader does when the integrity field has already been
    /// satisfied. CRC-32 is detection, not authentication, and anything with
    /// write access to the directory can produce a file that verifies — so the
    /// bounds below have to hold on their own.
    fn forge_plain_len(path: &Path, plain_len: u64) {
        let mut raw = fs::read(path).unwrap();
        raw[26..34].copy_from_slice(&plain_len.to_le_bytes());
        let mut crc = Crc32::new();
        crc.update(&raw[..SNAP_CRC_OFFSET]);
        crc.update(&raw[SNAP_HEADER_LEN..]);
        let checksum = crc.finish().to_le_bytes();
        raw[SNAP_CRC_OFFSET..SNAP_HEADER_LEN].copy_from_slice(&checksum);
        fs::write(path, &raw).unwrap();
    }

    /// §3.3, stated as a bound rather than as a hope.
    ///
    /// The header says ten plaintext bytes; the payload is a real zstd frame
    /// holding a whole state. The reader is bounded to `plain_len + 1`, so it
    /// stops eleven bytes in — it does not decompress the frame to find out how
    /// wrong the header was, which is the behaviour that made an unbounded
    /// loader a denial-of-service surface rather than a bug.
    #[test]
    fn a_payload_larger_than_its_declared_length_stops_at_the_bound() {
        let dir = tempfile::tempdir().unwrap();
        let path = save_snapshot(dir.path(), &bulky_state(3)).unwrap();
        forge_plain_len(&path, 10);

        match load_snapshot(&path).unwrap_err() {
            DbError::SnapshotCorrupt { reason, .. } => {
                assert!(
                    reason.contains("10 plaintext bytes") && reason.contains("11"),
                    "the bound must be what stopped it, and it must say so: {reason}"
                );
            }
            other => panic!("expected SnapshotCorrupt, got {other:?}"),
        }
    }

    /// The other direction, and the reason the check is an equality.
    ///
    /// A header claiming *more* than the frame holds cannot exhaust anything —
    /// the frame ends and the reader stops. Rejecting it anyway is what keeps
    /// the declared length a fact about the file rather than a ceiling: a
    /// reader that accepted a short frame under a large declaration would be
    /// accepting a truncated payload that happened to end on a frame boundary.
    #[test]
    fn a_payload_smaller_than_its_declared_length_is_refused_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = save_snapshot(dir.path(), &bulky_state(4)).unwrap();
        forge_plain_len(&path, u32::MAX as u64);

        match load_snapshot(&path).unwrap_err() {
            DbError::SnapshotCorrupt { reason, .. } => {
                assert!(reason.contains("plaintext bytes"), "{reason}");
            }
            other => panic!("expected SnapshotCorrupt, got {other:?}"),
        }
    }

    /// A declared length no allocator would survive must not reach an
    /// allocator.
    ///
    /// `u64::MAX` is the number a corrupt or hostile header reaches for, and
    /// the `saturating_add` in the reader is what keeps `plain_len + 1` from
    /// wrapping to zero and reading nothing at all. What bounds the work here
    /// is the frame itself, which ends where it ends — the failure is the
    /// length check afterwards, not an allocation.
    #[test]
    fn a_declared_length_of_u64_max_neither_wraps_nor_allocates() {
        let dir = tempfile::tempdir().unwrap();
        let path = save_snapshot(dir.path(), &bulky_state(5)).unwrap();
        forge_plain_len(&path, u64::MAX);

        match load_snapshot(&path).unwrap_err() {
            DbError::SnapshotCorrupt { reason, .. } => {
                assert!(
                    reason.contains(&format!("{} plaintext bytes", u64::MAX)),
                    "{reason}"
                );
            }
            other => panic!("expected SnapshotCorrupt, got {other:?}"),
        }
    }

    /// The checksum covers the header, so the forgery helper above has to be a
    /// forgery — if it did not recompute the field, every test using it would
    /// be passing for the wrong reason.
    #[test]
    fn doctoring_the_header_without_the_checksum_fails_earlier() {
        let dir = tempfile::tempdir().unwrap();
        let path = save_snapshot(dir.path(), &bulky_state(6)).unwrap();

        let mut raw = fs::read(&path).unwrap();
        raw[26..34].copy_from_slice(&10u64.to_le_bytes());
        fs::write(&path, &raw).unwrap();

        match load_snapshot(&path).unwrap_err() {
            DbError::SnapshotCorrupt { reason, .. } => {
                assert!(reason.contains("checksum mismatch"), "{reason}");
            }
            other => panic!("expected SnapshotCorrupt, got {other:?}"),
        }
    }

    /// The portability fact the `unix` branch rests on, asserted directly
    /// rather than through a snapshot write (0.13.13, W8.3).
    ///
    /// POSIX permits `fsync` on a directory descriptor to fail with `EINVAL`,
    /// and some filesystems take it up on that. If this platform were one of
    /// them, *every* `save_snapshot` would now fail — a large consequence for a
    /// call whose whole purpose is invisible when it works — so the question
    /// gets its own test with its own name.
    #[cfg(unix)]
    #[test]
    fn a_directory_handle_can_be_synced() {
        let dir = tempfile::tempdir().unwrap();
        sync_directory(dir.path()).expect("fsync on a directory descriptor");
    }

    /// Off unix this does nothing, and *nothing* is the behaviour under test
    /// (0.13.13, W8.3).
    ///
    /// A path that does not exist would be an error from any implementation
    /// that touched the filesystem, so a green here says the branch really is
    /// inert — which is what the docs claim, and a claim about a no-op is the
    /// kind that rots quietly if nobody writes it down as an assertion.
    #[cfg(not(unix))]
    #[test]
    fn the_directory_sync_is_inert_off_unix() {
        let dir = tempfile::tempdir().unwrap();
        sync_directory(&dir.path().join("no-such-directory"))
            .expect("the non-unix branch has nothing that can fail");
    }

    /// The publish step is a rename, not a copy: a `.tmp` surviving a
    /// successful save would mean the file at the final name got there some
    /// other way, and the atomicity W8.3 makes durable would be gone with it.
    #[test]
    fn a_completed_save_leaves_no_temporary_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = save_snapshot(dir.path(), &bulky_state(7)).unwrap();
        assert!(path.exists(), "the snapshot is at its final name");

        let leftovers: Vec<PathBuf> = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("tmp"))
            .collect();
        assert!(leftovers.is_empty(), "left behind {leftovers:?}");
    }

    // -----------------------------------------------------------------------
    // The deterministic half of W8.4 (0.13.14, D-187)
    //
    // `cargo-fuzz` needs nightly and libFuzzer and does not run on Windows, so
    // it runs in CI and nowhere else. These do the same job on every platform
    // and in every `cargo test`, exhaustively rather than randomly, and they
    // are what a finding from the fuzzer would be pinned as.
    // -----------------------------------------------------------------------

    /// Small enough that flipping every bit of the container is cheap, and not
    /// so small that the payload is a single zstd literal block.
    fn modest_state(seq: i64) -> MaterializedState {
        let mut concepts = HashMap::new();
        for i in 0..8u32 {
            concepts.insert(
                format!("c{i}"),
                NodeAttributes {
                    id: format!("c{i}"),
                    title: format!("concept {i}"),
                    content: format!("some content for {i} ").repeat(3),
                    embedding_model: (i % 2 == 0).then(|| "model-a".to_string()),
                },
            );
        }
        MaterializedState {
            seq_anchor: seq,
            timestamp: TS.to_string(),
            concepts,
            edges: vec![(
                "c0".to_string(),
                "c1".to_string(),
                "relates_to".to_string(),
                TS.to_string(),
                "A".to_string(),
            )],
            predates_recorded_history: false,
        }
    }

    /// Every failure a reader may report about a *file*. Anything else — a
    /// panic, or an error naming the ledger — is the finding.
    fn assert_named_refusal(what: &str, err: DbError) {
        match err {
            DbError::SnapshotCorrupt { .. } | DbError::SnapshotIncompatible { .. } => {}
            other => panic!("{what}: expected a named snapshot error, got {other:?}"),
        }
    }

    /// The container's whole promise, asserted exhaustively rather than
    /// sampled: **change any bit of a snapshot and it is refused.**
    ///
    /// That this holds is not luck. Bytes 0..34 and the payload are under the
    /// checksum, bytes 34..38 *are* the checksum, and CRC-32 detects every
    /// single-bit error by construction — so there is no byte of the file where
    /// a flip can go unnoticed, and the test says so for all of them instead of
    /// asserting it for the three a hand-written case would have picked.
    ///
    /// The clean parse first is deliberate. A fixture that does not load makes
    /// every assertion below pass for the wrong reason, which is exactly how
    /// [D-054](../../docs/architecture/s13-decision-register.md#d-054)'s
    /// retention tests spent a release exercising a path they did not name.
    #[test]
    fn every_single_bit_flip_in_a_snapshot_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = save_snapshot(dir.path(), &modest_state(1)).unwrap();
        let clean = fs::read(&path).unwrap();

        parse_snapshot("clean", &clean)
            .expect("the fixture must load, or nothing below means anything");

        let mut refused = 0usize;
        for byte in 0..clean.len() {
            for bit in 0..8u8 {
                let mut damaged = clean.clone();
                damaged[byte] ^= 1 << bit;
                match parse_snapshot("damaged", &damaged) {
                    Ok(_) => panic!("bit {bit} of byte {byte} changed and the file still loaded"),
                    Err(e) => {
                        assert_named_refusal(&format!("bit {bit} of byte {byte}"), e);
                        refused += 1;
                    }
                }
            }
        }
        assert_eq!(refused, clean.len() * 8, "every bit of the file was tried");
    }

    /// Every prefix of a snapshot is refused, and so is every snapshot with
    /// anything appended to it.
    ///
    /// Truncation is the shape an atomic rename was supposed to make
    /// impossible ([D-043](../../docs/architecture/s13-decision-register.md#d-043))
    /// and a filesystem that loses the tail of a file it acknowledged can still
    /// produce. Trailing bytes are the shape a partially-overwritten file
    /// takes. Both are caught by the declared length before anything is
    /// hashed, which is why they are cheap enough to test for every length.
    #[test]
    fn every_truncation_and_every_extension_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = save_snapshot(dir.path(), &modest_state(2)).unwrap();
        let clean = fs::read(&path).unwrap();

        for cut in 0..clean.len() {
            match parse_snapshot("cut", &clean[..cut]) {
                Ok(_) => panic!("a {cut}-byte prefix loaded as a whole snapshot"),
                Err(e) => assert_named_refusal(&format!("{cut}-byte prefix"), e),
            }
        }

        for extra in [1usize, 7, 64, 4096] {
            let mut grown = clean.clone();
            grown.extend(std::iter::repeat_n(0u8, extra));
            match parse_snapshot("grown", &grown) {
                Ok(_) => panic!("{extra} appended bytes went unnoticed"),
                Err(e) => assert_named_refusal(&format!("{extra} appended bytes"), e),
            }
        }
    }

    /// The half a fuzzer cannot reach on its own: **arbitrary bytes behind a
    /// checksum that agrees with them.**
    ///
    /// `wrap_plaintext` recomputes the header and the CRC, so every case here
    /// clears steps 1–3 of the reader and lands squarely on zstd and bincode,
    /// which is where W8.2's bounds live and where a panic would be a real
    /// defect. Some of these deserialize into a perfectly valid — and quite
    /// wrong — `MaterializedState`, which is not a failure: nothing in this
    /// format claims to detect damage that arrives with a correct checksum, and
    /// [D-185](../../docs/architecture/s13-decision-register.md#d-185) says so
    /// in as many words. The property is that the reader answers rather than
    /// dies.
    #[test]
    fn arbitrary_plaintext_behind_a_valid_checksum_never_panics() {
        let plain = bincode::serialize(&modest_state(3)).unwrap();

        let mut answered = 0usize;
        for byte in 0..plain.len() {
            for bit in [0u8, 3, 7] {
                let mut mutated = plain.clone();
                mutated[byte] ^= 1 << bit;
                match parse_snapshot("wrapped", &wrap_plaintext(&mutated)) {
                    Ok(_) => answered += 1,
                    Err(e) => {
                        assert_named_refusal(&format!("bit {bit} of plaintext byte {byte}"), e);
                        answered += 1;
                    }
                }
            }
        }
        assert_eq!(answered, plain.len() * 3);

        // Shapes a bit flip cannot produce: nothing at all, a run of zeros, and
        // a plaintext far longer than any state this fixture describes.
        for odd in [vec![], vec![0u8; 1], vec![0u8; 4096], vec![0xFFu8; 64]] {
            match parse_snapshot("odd", &wrap_plaintext(&odd)) {
                Ok(_) => {}
                Err(e) => assert_named_refusal("an odd plaintext", e),
            }
        }
    }

    /// A decompression bomb with a **correct** checksum, which is the one
    /// damaged input this format cannot detect by hashing and has to refuse by
    /// arithmetic (0.13.14, W8.4).
    ///
    /// 64 MiB of zeros compresses to a few hundred bytes. The container built
    /// around it here is entirely well-formed — magic, versions, both lengths
    /// and a CRC that agrees with every byte — and it declares a plaintext of
    /// 1,024 bytes. A reader that decompressed first and checked afterwards
    /// would allocate the full 64 MiB to discover that; the `take(plain_len +
    /// 1)` bound stops it 65,535 KiB short, which is what the reported length
    /// in the error proves.
    ///
    /// This is the "never an allocation storm" half of W8.4 asserted where a
    /// deterministic test can assert it. The other half — arbitrary frames,
    /// arbitrary declared lengths — is `fuzz_targets/snapshot_frame.rs` under
    /// libFuzzer's own `-malloc_limit_mb`, which is the tool built for it.
    #[test]
    fn a_decompression_bomb_with_a_valid_checksum_stops_at_the_declared_length() {
        let bomb = zstd::encode_all(&vec![0u8; 64 * 1024 * 1024][..], 3).unwrap();
        assert!(
            bomb.len() < 64 * 1024,
            "the fixture must actually be a bomb"
        );

        let container = wrap_payload(&bomb, 1024);
        match parse_snapshot("bomb", &container).unwrap_err() {
            DbError::SnapshotCorrupt { reason, .. } => {
                assert!(
                    reason.contains("1024 plaintext bytes") && reason.contains("1025"),
                    "the reader should stop one byte past the declared length: {reason}"
                );
            }
            other => panic!("expected SnapshotCorrupt, got {other:?}"),
        }
    }
}
