//! CRC-32 (IEEE 802.3), for the snapshot container's integrity field
//! (0.13.12, W8.2, D-185).
//!
//! # Why this is here rather than a dependency
//!
//! Forty lines against a crate, for a polynomial that has not changed since
//! 1975 and a table this file can generate at compile time. `crc32fast` is a
//! good crate and would be faster on long inputs — it uses SSE4.2 where the
//! target has it — but the input here is one snapshot payload per save and per
//! load, on a path already dominated by zstd and bincode, and a dependency is
//! a thing that has to be audited, updated and kept compatible for as long as
//! the project lives. The measurement that would justify it does not exist,
//! and the table below is checked against the published test vector.
//!
//! # What it is and is not for
//!
//! Detection of accidental damage: truncation, a flipped bit, a partial write
//! surviving where the atomic rename was supposed to prevent one. CRC-32 is
//! **not** a cryptographic hash and this is not authentication — anyone able to
//! write a forged snapshot into the directory can compute the field, and can
//! also write to the database file itself. See
//! [D-185](../../docs/architecture/s13-decision-register.md#d-185) for the
//! threat model that makes that acceptable.

/// The reflected IEEE polynomial, which is how every table-driven CRC-32
/// implementation writes 0x04C1_1DB7.
const POLYNOMIAL: u32 = 0xEDB8_8320;

/// Generated at compile time, so there is no table in the source to get wrong
/// by transcription and none to build at startup.
const TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                POLYNOMIAL ^ (crc >> 1)
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

/// A running CRC-32 over one or more slices.
///
/// The snapshot checksum covers the header and the payload, which live in
/// different places at write time, so this accumulates rather than taking a
/// single slice.
pub(crate) struct Crc32(u32);

impl Crc32 {
    pub(crate) fn new() -> Self {
        Self(0xFFFF_FFFF)
    }

    pub(crate) fn update(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            let index = ((self.0 ^ byte as u32) & 0xFF) as usize;
            self.0 = TABLE[index] ^ (self.0 >> 8);
        }
    }

    pub(crate) fn finish(self) -> u32 {
        self.0 ^ 0xFFFF_FFFF
    }
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The published check value: CRC-32 of the nine bytes "123456789" is
    /// 0xCBF43926. If the table generator or the update loop is wrong in any
    /// way that matters, this is what says so.
    #[test]
    fn the_standard_check_vector_matches() {
        let mut crc = Crc32::new();
        crc.update(b"123456789");
        assert_eq!(crc.finish(), 0xCBF4_3926);
    }

    /// Accumulating in pieces must equal hashing the whole, or the snapshot's
    /// header-then-payload call would not be checking what a reader recomputes.
    #[test]
    fn splitting_the_input_does_not_change_the_result() {
        let mut whole = Crc32::new();
        whole.update(b"123456789");

        let mut split = Crc32::new();
        split.update(b"1234");
        split.update(b"");
        split.update(b"56789");

        assert_eq!(whole.finish(), split.finish());
    }

    /// The property the format relies on: one flipped bit anywhere changes the
    /// value. CRC-32 guarantees this for any single-bit error, so the loop is
    /// over every bit of a payload the size of a small snapshot header.
    #[test]
    fn every_single_bit_flip_is_detected() {
        let clean: Vec<u8> = (0u8..64).collect();
        let mut baseline = Crc32::new();
        baseline.update(&clean);
        let baseline = baseline.finish();

        for byte in 0..clean.len() {
            for bit in 0..8 {
                let mut damaged = clean.clone();
                damaged[byte] ^= 1 << bit;
                let mut crc = Crc32::new();
                crc.update(&damaged);
                assert_ne!(
                    crc.finish(),
                    baseline,
                    "flipping bit {bit} of byte {byte} went unnoticed"
                );
            }
        }
    }

    /// Zero bytes and zeroed bytes are different things, and the format
    /// depends on it: a file that was never finished being written looks like
    /// a run of zeros, and its checksum field would be zero too. The empty
    /// input hashes to 0 by definition, but any actual zero *bytes* do not, so
    /// a truncated payload cannot accidentally agree with a zeroed field.
    #[test]
    fn zeroed_bytes_do_not_hash_to_zero() {
        assert_eq!(
            Crc32::new().finish(),
            0,
            "the empty input is 0 by definition"
        );
        let mut crc = Crc32::new();
        crc.update(&[0u8; 4]);
        assert_ne!(crc.finish(), 0, "four zero bytes must not hash to zero");
    }
}
