pub mod connection;
pub mod error;
pub mod graph;
pub mod integrity;
pub mod metrics;
pub mod schema;
pub mod temporal;
pub mod util;
pub mod vector;

// `CHUNK_BUDGET` is re-exported at the root because four modules already write
// `[`crate::CHUNK_BUDGET`]` in their rustdoc — an intra-doc link that resolved
// to nothing, since the const lives in `connection`. The bound is the crate's
// one cross-cutting number; the root is where a reader looks for it.
pub use connection::{Annotation, ConceptUpsert, Database, CHUNK_BUDGET};
pub use error::{DbError, Overlap, Result};

pub mod prelude {
    pub use crate::connection::{
        chunk_rows, estimated_bulk_hold, Annotation, ConceptUpsert, Database,
        BULK_ATOMIC_WARN_HOLD, CHUNK_BUDGET, MAX_ARCHIVE_SESSIONS,
    };
    pub use crate::error::{DbError, Overlap, Result};
    pub use crate::graph::{
        AttributeMode, CandidateCount, CostEstimate, CostEstimator, EdgeAssertion,
        FilteredVectorSearch, TraversalBuilder, VectorFilterStrategy,
    };
    pub use crate::integrity::{audit_current, rebuild_current, RebuildReport};
    pub use crate::metrics::CommandKind;
    #[cfg(feature = "metrics")]
    pub use crate::metrics::{KindSnapshot, MetricsSnapshot};
    pub use crate::temporal::{
        archive, query_as_of_edges, reconstruct, ArchiveReport, Interval, MaterializedState,
        SnapshotCadence,
    };
    pub use crate::util::{Clock, FakeClock, SystemClock};
    pub use crate::vector::{
        declared_dimension, escape_fts5_query, keyword_search, reciprocal_rank_fusion,
        register_model, registered_models, search_vector, upsert_embedding, EmbeddingCodec,
        HybridHit, HybridSearch, ModelName, VectorSearchResult, RRF_K,
    };
}
