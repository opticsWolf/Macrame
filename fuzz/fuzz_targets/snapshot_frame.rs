//! zstd and the declared-length bound: arbitrary payload bytes under an
//! arbitrary declared plaintext length, with a checksum that agrees with both
//! (0.13.14, W8.4, D-187).
//!
//! The layer between the other two targets, and the one where "never an
//! allocation storm" is a real question rather than a rhetorical one. Every
//! other input this format can receive is caught by the checksum; a
//! decompression bomb is not, because a bomb's checksum is *correct* — the file
//! is exactly what its author intended it to be. All that stands between the
//! reader and the frame's full expansion is `take(plain_len + 1)`, and
//! `plain_len` is a number off a disk.
//!
//! Input layout: the first eight bytes are the declared plaintext length,
//! little-endian, and the rest is the payload. That split is deliberate rather
//! than incidental — it lets the mutator move the declared length and the bytes
//! it describes independently, which is the pair the bound has to hold across.
//! Inputs shorter than eight bytes are skipped; there is nothing to declare.
//!
//! Run this one with a tight `-malloc_limit_mb`. An allocation storm here is
//! the finding, and libFuzzer reporting it as an OOM with the input attached is
//! the reason this is a fuzz target and not another unit test.

#![no_main]

use libfuzzer_sys::fuzz_target;
use macrame::error::DbError;
use macrame::temporal::fuzzing;

fuzz_target!(|data: &[u8]| {
    if data.len() < 8 {
        return;
    }
    let (len_bytes, payload) = data.split_at(8);
    let plain_len = u64::from_le_bytes(len_bytes.try_into().expect("split_at(8)"));

    let container = fuzzing::wrap_payload(payload, plain_len);
    match fuzzing::parse(&container) {
        Ok(_) => {}
        Err(DbError::SnapshotCorrupt { .. }) => {}
        Err(other) => panic!("a container this target built is not foreign or unreadable: {other:?}"),
    }
});
