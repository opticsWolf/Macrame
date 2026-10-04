v0.18.0 : 1845 lines, 17 modules, 788 distinct items
working : 1918 lines, 18 modules, 819 distinct items
net lines: +73   net items: +31

--- modules (17 -> 18) ---
demoted: 0   new: 1
  + blob

--- non_exhaustive (45 -> 47) ---
  + pub enum BlobLocation
  + pub struct BlobStat

--- surplus paths on items present in both: 892 -> 933 ---

=== REMOVED FROM THE SURFACE: 0 ===

=== ADDED TO THE SURFACE: 31 ===
  pub AbortKind::BlobImmutable
  pub ArchiveReport::blob_scan_bytes: u64
  pub ArchiveReport::blobs_archived: usize
  pub ArchiveReport::blobs_restored: usize
  pub BlobLocation::Cold
  pub BlobLocation::Hot
  pub BlobStat::location: BlobLocation
  pub BlobStat::put_at: alloc::string::String
  pub BlobStat::sha256: alloc::string::String
  pub BlobStat::size: u64
  pub CommandKind::BlobPut
  pub DbError::BlobTooLarge
  pub DbError::BlobTooLarge::max: usize
  pub DbError::BlobTooLarge::size: usize
  pub DbError::InvalidDigest(alloc::string::String)
  pub Tuning::max_blob_bytes: core::option::Option<usize>
  pub async fn Database::blob_get(&self, &str) -> Result<core::option::Option<alloc::vec::Vec<u8>>>
  pub async fn Database::blob_put(&self, &[u8]) -> Result<alloc::string::String>
  pub async fn Database::blob_stat(&self, &str) -> Result<core::option::Option<BlobStat>>
  pub const ABORT_BLOB_IMMUTABLE: &str
  pub const BLOBS_PUT_AT_INDEX: &str
  pub const BLOB_WARN_HOLD: core::time::Duration
  pub const CREATE_BLOBS_GUARD_DELETE: &str
  pub const CREATE_BLOBS_GUARD_UPDATE: &str
  pub const CREATE_BLOBS_TABLE: &str
  pub const DEFAULT_MAX_BLOB_BYTES: usize
  pub const DIGEST_HEX_LEN: usize
  pub enum BlobLocation
  pub fn Tuning::max_blob_bytes(self, usize) -> Self
  pub fn validate_digest(&str) -> Result<()>
  pub struct BlobStat
