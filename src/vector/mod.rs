pub(crate) mod embedding;
pub(crate) mod hybrid;
pub(crate) mod model;
pub(crate) mod registry;
pub(crate) mod search;

pub use embedding::EmbeddingCodec;
pub use hybrid::{escape_fts5_query, keyword_search, HybridHit, HybridSearch, RRF_K};
pub use model::ModelName;
pub use registry::{declared_dimension, register_model, registered_models};
pub use search::{reciprocal_rank_fusion, search_vector, upsert_embedding, VectorSearchResult};
