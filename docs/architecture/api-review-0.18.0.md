v0.17.0 : 1790 lines, 16 modules, 762 distinct items
working : 1845 lines, 17 modules, 788 distinct items
net lines: +55   net items: +26

--- modules (16 -> 17) ---
demoted: 0   new: 1
  + kv

--- non_exhaustive (45 -> 45) ---

--- surplus paths on items present in both: 864 -> 892 ---

=== REMOVED FROM THE SURFACE: 0 ===

=== ADDED TO THE SURFACE: 26 ===
  pub CommandKind::KvWrite
  pub CommandKind::RegisterExtraIndex
  pub ConceptUpsert::extra: core::option::Option<alloc::string::String>
  pub DbError::InvalidExtra
  pub DbError::InvalidExtra::id: alloc::string::String
  pub DbError::InvalidExtra::reason: alloc::string::String
  pub DbError::InvalidExtraPath(alloc::string::String)
  pub DbError::InvalidKvKey(alloc::string::String)
  pub NodeAttributes::extra: alloc::string::String
  pub TraversalBuilder::extra: bool
  pub async fn Database::kv_delete(&self, impl core::convert::Into<alloc::string::String>) -> Result<bool>
  pub async fn Database::kv_get(&self, &str) -> Result<core::option::Option<alloc::string::String>>
  pub async fn Database::kv_put(&self, impl core::convert::Into<alloc::string::String>, impl core::convert::Into<alloc::string::String>) -> Result<()>
  pub async fn Database::kv_scan(&self, &str, usize) -> Result<alloc::vec::Vec<(alloc::string::String, alloc::string::String)>>
  pub async fn Database::register_extra_index(&self, &str) -> Result<()>
  pub const CREATE_KV_STORE_TABLE: &str
  pub const EXTRA_LAYER_INDEX: &str
  pub const MAX_EXTRA_BYTES: usize
  pub const MAX_KV_KEY: usize
  pub fn ConceptUpsert::extra(self, impl core::convert::Into<alloc::string::String>) -> Self
  pub fn NodeAttributes::extra(self, impl core::convert::Into<alloc::string::String>) -> Self
  pub fn NodeData::extra(&self) -> core::option::Option<&str>
  pub fn NodeData::with_extra(self, impl core::convert::Into<alloc::string::String>) -> Self
  pub fn TraversalBuilder::extra(self, bool) -> Self
  pub fn validate_kv_key(&str) -> Result<()>
  pub fn validate_kv_prefix(&str) -> Result<()>
