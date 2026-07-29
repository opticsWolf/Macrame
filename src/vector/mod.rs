pub mod embedding;
pub mod model;
pub mod registry;
pub mod search;

pub use embedding::EmbeddingCodec;
pub use model::ModelName;
pub use registry::{declared_dimension, register_model, registered_models};
pub use search::{
    reciprocal_rank_fusion, search_vector, upsert_embedding, VectorSearchResult,
};
