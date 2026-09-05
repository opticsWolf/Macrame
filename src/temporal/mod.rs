pub(crate) mod archive;
pub(crate) mod as_of;
pub(crate) mod interval;
pub(crate) mod replay;
pub(crate) mod snapshot;

pub use archive::{archivable_concepts, archive, rehydrate, ArchiveReport, RehydrateReport};
pub use as_of::{
    hydrate_attributes, query_as_of_edges, query_as_of_edges_on, AsOf, NodeAttributes,
};
pub use interval::Interval;
pub use replay::{
    reconstruct, reconstruct_on, resolve_beliefs, verify_snapshot_chain, ChainCheck, EdgeBelief,
    MaterializedState,
};
pub use snapshot::{
    cleanup_expired_snapshots, load_snapshot, save_snapshot, write_final, SnapshotCadence,
};

// `fuzz/` is a separate workspace and the one consumer outside this crate that
// `pub(crate) mod snapshot` would have locked out. It moves with its module
// rather than keeping `snapshot` public for it: `#[doc(hidden)]` and
// feature-gated, so `cargo-public-api` never reported it and the tracked
// surface is the same either way (D-208).
#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub use snapshot::fuzzing;
