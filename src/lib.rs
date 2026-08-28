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
pub use connection::{
    Annotation, BulkControl, BulkProgress, CadencePolicy, CancelToken, CheckpointReport,
    ConceptUpsert, Database, Tuning, WalCheckpointPolicy, CHUNK_BUDGET,
};
pub use error::{BulkInterrupted, BulkResult, DbError, Overlap, Result, StatedInstants};
pub use util::{FutureStampPolicy, DEFAULT_FUTURE_STAMP_TOLERANCE};

pub mod prelude {
    // Names, not namespaces (D-208). `chunk_rows` was in this list and is a
    // *module*, so `prelude::chunk_rows::EDGES` was a second canonical path to
    // all four constants and a second name for the module itself. A prelude
    // exists so one `use` brings in the names a caller needs; it is not a
    // second directory tree.
    pub use crate::connection::{
        estimated_bulk_hold, Annotation, BulkControl, BulkProgress, CadencePolicy, CancelToken,
        CheckpointReport, ConceptUpsert, Database, Tuning, WalCheckpointPolicy,
        BULK_ATOMIC_WARN_HOLD, CHUNK_BUDGET, MAX_ARCHIVE_SESSIONS,
    };
    pub use crate::error::{BulkInterrupted, BulkResult, DbError, Overlap, Result, StatedInstants};
    pub use crate::graph::{
        AttributeMode, CandidateCount, CostEstimate, CostEstimator, EdgeAssertion,
        FilteredVectorSearch, TraversalBuilder, VectorFilterStrategy,
    };
    pub use crate::integrity::{audit_current, rebuild_current, RebuildReport};
    pub use crate::metrics::CommandKind;
    #[cfg(feature = "metrics")]
    pub use crate::metrics::{KindSnapshot, MetricsSnapshot};
    // `archive` names both a module and a function in `temporal`, and an
    // explicit import binds the name in *both* namespaces. Listing it here
    // re-exported the module too — and once that module is `pub(crate)`, this
    // `pub use` republishes it as public at `prelude::archive`, with neither an
    // error nor a warning. Naming the function's own path is what imports the
    // function alone (D-208).
    pub use crate::temporal::archive::archive;
    pub use crate::temporal::{
        query_as_of_edges, query_as_of_edges_on, reconstruct, ArchiveReport, Interval,
        MaterializedState, SnapshotCadence,
    };
    pub use crate::util::{
        Clock, FakeClock, FutureStampPolicy, SystemClock, DEFAULT_FUTURE_STAMP_TOLERANCE,
    };
    pub use crate::vector::{
        declared_dimension, escape_fts5_query, keyword_search, reciprocal_rank_fusion,
        register_model, registered_models, search_vector, upsert_embedding, EmbeddingCodec,
        HybridHit, HybridSearch, ModelName, VectorSearchResult, RRF_K,
    };
}
