//! The container as a whole: arbitrary bytes offered as a snapshot file
//! (0.13.14, W8.4, D-187).
//!
//! Closes §5.4's outer half. The property is the one the plan states: a corrupt
//! input produces a **named** error, never a panic. `load_snapshot` has exactly
//! two ways to say no — `SnapshotCorrupt` for damage and `SnapshotIncompatible`
//! for a file another build wrote — so this target is also a guard on that
//! surface: a third variant appearing here is a finding, because a caller who
//! matched on the two would silently stop handling something.
//!
//! **This target spends most of its budget at the checksum, and that is
//! expected rather than a flaw.** Coverage-guided mutation finds the four-byte
//! magic quickly and does not find a CRC-32 over 34 header bytes plus the
//! payload, so almost every input dies at step two. What that exercises is the
//! framing: magic, versions, declared lengths, the arithmetic on numbers read
//! off a disk. The layers behind the checksum have targets of their own
//! (`snapshot_payload`, `snapshot_frame`), because reaching them by mutation is
//! not something a fuzzer can be expected to do.

#![no_main]

use libfuzzer_sys::fuzz_target;
use macrame::error::DbError;
use macrame::temporal::snapshot::fuzzing;

fuzz_target!(|data: &[u8]| {
    match fuzzing::parse(data) {
        Ok(_) => {}
        Err(DbError::SnapshotCorrupt { .. }) | Err(DbError::SnapshotIncompatible { .. }) => {}
        Err(other) => panic!("a file produced an error that is not about a file: {other:?}"),
    }
});
