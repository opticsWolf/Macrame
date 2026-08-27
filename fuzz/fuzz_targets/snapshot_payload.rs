//! `bincode`'s decoder, reached by wrapping arbitrary plaintext in a container
//! that is correct in every other respect (0.13.14, W8.4, D-187).
//!
//! The input is the **plaintext**: this target compresses it and builds the
//! header and checksum around it, so every case clears the framing, clears the
//! checksum, clears zstd, and lands on `MaterializedState`'s deserializer with
//! the `with_limit` W8.2 put there. That is the component §3.3 named — a
//! deserializer walking a corrupt stream, allocating as it goes — and it is
//! unreachable by mutating whole files, which is why it gets a door of its own.
//!
//! **A successful parse is not a finding.** Mutated plaintext frequently
//! deserializes into a perfectly valid and completely wrong
//! `MaterializedState`. Nothing in this format claims otherwise: the checksum
//! detects damage that arrives *without* a correct checksum, and D-185 states
//! the threat model that makes that acceptable. What is being asserted is that
//! the reader answers — with a state or with a named error — rather than
//! panicking or running away with the machine's memory. The second half is
//! libFuzzer's `-malloc_limit_mb`, which the CI job sets deliberately low.

#![no_main]

use libfuzzer_sys::fuzz_target;
use macrame::error::DbError;
use macrame::temporal::fuzzing;

fuzz_target!(|data: &[u8]| {
    let container = fuzzing::wrap_plaintext(data);
    match fuzzing::parse(&container) {
        Ok(_) => {}
        Err(DbError::SnapshotCorrupt { .. }) => {}
        Err(other) => panic!("a container this target built is not foreign or unreadable: {other:?}"),
    }
});
