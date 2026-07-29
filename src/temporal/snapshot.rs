use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::error::{DbError, Result};
use crate::temporal::replay::MaterializedState;

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
/// **v2 (0.5.5)** adds the snapshot's own instant to the header (D-054).
const SNAP_FORMAT_VERSION: u16 = 2;

/// `magic (4) + format_version (2) + schema_version (4) + taken_at_micros (8)`,
/// little-endian.
const SNAP_HEADER_LEN: usize = 18;

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

fn snapshot_header(schema_version: u32, taken_at: u64) -> [u8; SNAP_HEADER_LEN] {
    let mut h = [0u8; SNAP_HEADER_LEN];
    h[0..4].copy_from_slice(&SNAP_MAGIC);
    h[4..6].copy_from_slice(&SNAP_FORMAT_VERSION.to_le_bytes());
    h[6..10].copy_from_slice(&schema_version.to_le_bytes());
    h[10..18].copy_from_slice(&taken_at.to_le_bytes());
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

/// Save a bincode-serialized, zstd-compressed snapshot file (.snap.zst) (§5.5).
///
/// Written to a temporary file, flushed to disk, and renamed into place. A
/// snapshot is read back with no integrity check beyond what zstd and bincode
/// happen to notice, so a half-written file at the final name is a file that
/// looks loadable and is not — and it would be the *newest* one, which is
/// exactly the one a restart reaches for. Rename within a directory is atomic,
/// so a crash leaves either the old snapshot or the new one, never a splice.
pub fn save_snapshot(snapshots_dir: &Path, state: &MaterializedState) -> Result<PathBuf> {
    let fail = |what: &str, e: std::io::Error| DbError::ReplayCorrupt {
        seq: state.seq_anchor,
        reason: format!("{what}: {e}"),
    };

    fs::create_dir_all(snapshots_dir).map_err(|e| fail("failed to create snapshot directory", e))?;

    let path = snapshots_dir.join(snapshot_filename(state.seq_anchor));
    let tmp_path = path.with_extension("tmp");

    let serialized = bincode::serialize(state).map_err(|e| DbError::ReplayCorrupt {
        seq: state.seq_anchor,
        reason: format!("failed to serialize snapshot: {e}"),
    })?;

    let compressed = zstd::encode_all(&serialized[..], 3)
        .map_err(|e| fail("failed to compress snapshot", e))?;

    let mut file =
        fs::File::create(&tmp_path).map_err(|e| fail("failed to create snapshot temp file", e))?;
    // Header first, uncompressed: it has to be readable without committing to
    // decompressing a payload this build may not understand (D-043).
    file.write_all(&snapshot_header(
        crate::schema::migrations::SCHEMA_VERSION,
        taken_at_micros(state),
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
pub fn load_snapshot(path: &Path) -> Result<MaterializedState> {
    let mut file = fs::File::open(path).map_err(|e| DbError::ReplayCorrupt {
        seq: 0,
        reason: format!("Failed to open snapshot file {:?}: {e}", path),
    })?;

    let mut raw = Vec::new();
    file.read_to_end(&mut raw).map_err(|e| DbError::ReplayCorrupt {
        seq: 0,
        reason: format!("Failed to read snapshot file {:?}: {e}", path),
    })?;

    if raw.len() < SNAP_HEADER_LEN || raw[0..4] != SNAP_MAGIC {
        return Err(DbError::SnapshotIncompatible {
            path: path.display().to_string(),
            reason: "not a macrame snapshot, or written before the versioned \
                     container existed (0.5.4 and earlier)"
                .to_string(),
        });
    }

    let format = u16::from_le_bytes([raw[4], raw[5]]);
    let schema = u32::from_le_bytes([raw[6], raw[7], raw[8], raw[9]]);
    let expected_schema = crate::schema::migrations::SCHEMA_VERSION;
    if format != SNAP_FORMAT_VERSION || schema != expected_schema {
        return Err(DbError::SnapshotIncompatible {
            path: path.display().to_string(),
            reason: format!(
                "snapshot is format v{format}/schema v{schema}; this build reads \
                 format v{SNAP_FORMAT_VERSION}/schema v{expected_schema}"
            ),
        });
    }

    let compressed = &raw[SNAP_HEADER_LEN..];
    let decompressed = zstd::decode_all(compressed).map_err(|e| DbError::ReplayCorrupt {
        seq: 0,
        reason: format!("Failed to decompress snapshot {:?}: {e}", path),
    })?;

    let state: MaterializedState = bincode::deserialize(&decompressed).map_err(|e| DbError::ReplayCorrupt {
        seq: 0,
        reason: format!("Failed to deserialize snapshot {:?}: {e}", path),
    })?;

    Ok(state)
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
pub async fn write_final(
    conn: &libsql::Connection,
    snapshots_dir: &Path,
    ts: &str,
    archive_path: Option<&Path>,
) -> Result<PathBuf> {
    let state =
        crate::temporal::replay::reconstruct(conn, ts, archive_path, Some(snapshots_dir)).await?;
    let path = save_snapshot(snapshots_dir, &state)?;
    cleanup_expired_snapshots(snapshots_dir)?;
    Ok(path)
}

/// Snapshots kept unconditionally, newest first, by [`cleanup_expired_snapshots`] (§5.5).
const RETAIN: usize = 5;

/// Days for which one snapshot each is kept beyond [`RETAIN`] (§5.5, D-054).
const RETAIN_DAYS: i64 = 30;

const MICROS_PER_DAY: u64 = 86_400_000_000;

/// Retention: the newest [`RETAIN`], **plus one per day for [`RETAIN_DAYS`]**
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
/// Parsing removes the dependency on [`SEQ_WIDTH`] entirely.
///
/// A snapshot whose header carries no readable instant survives only under the
/// newest-[`RETAIN`] rule. That is deliberate: it is a file this build would
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
