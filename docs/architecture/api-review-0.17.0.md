v0.16.0 : 1772 lines, 16 modules, 755 distinct items
working : 1790 lines, 16 modules, 762 distinct items
net lines: +18   net items: +7

--- modules (16 -> 16) ---
demoted: 0   new: 0

--- non_exhaustive (45 -> 45) ---

--- surplus paths on items present in both: 853 -> 864 ---

=== REMOVED FROM THE SURFACE: 0 ===

=== ADDED TO THE SURFACE: 7 ===
  pub CommandKind::DropEmbeddingIndex
  pub CommandKind::LinksCurrentMirror
  pub CommandKind::RebuildEmbeddingIndex
  pub async fn Database::bulk_embeddings(&self, &ModelName, alloc::vec::Vec<(alloc::string::String, alloc::vec::Vec<f32>)>) -> BulkResult<usize>
  pub async fn Database::bulk_embeddings_with(&self, &ModelName, alloc::vec::Vec<(alloc::string::String, alloc::vec::Vec<f32>)>, BulkControl) -> BulkResult<usize>
  pub async fn Database::bulk_import_deferred(&self, alloc::vec::Vec<EdgeAssertion>) -> BulkResult<usize>
  pub async fn Database::bulk_import_deferred_with(&self, alloc::vec::Vec<EdgeAssertion>, BulkControl) -> BulkResult<usize>
