pub mod connection;
pub mod error;
pub mod graph;
pub mod integrity;
pub mod schema;
pub mod temporal;
pub mod util;
pub mod vector;

pub use connection::{Annotation, ConceptUpsert, Database};
pub use error::{DbError, Result};

pub mod prelude {
    pub use crate::connection::{Annotation, ConceptUpsert, Database, CHUNK_ROWS};
    pub use crate::error::{DbError, Result};
    pub use crate::graph::{
        AttributeMode, CandidateCount, CostEstimate, CostEstimator, EdgeAssertion,
        FilteredVectorSearch, TraversalBuilder, VectorFilterStrategy,
    };
    pub use crate::integrity::{audit_current, rebuild_current, RebuildReport};
    pub use crate::temporal::{
        archive, query_as_of_edges, reconstruct, ArchiveReport, Interval, MaterializedState,
    };
    pub use crate::util::{Clock, FakeClock, SystemClock};
    pub use crate::vector::{
        declared_dimension, escape_fts5_query, keyword_search, reciprocal_rank_fusion,
        register_model, registered_models, search_vector, upsert_embedding, EmbeddingCodec,
        HybridHit, HybridSearch, ModelName, VectorSearchResult, RRF_K,
    };
}
