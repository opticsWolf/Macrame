/// Strategy for combining vector search and graph traversal filters (§5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VectorFilterStrategy {
    /// Post-filtering: vector top-k search first, then filter reachable graph nodes.
    PostFilter,
    /// Pre-filtering via CTE: graph traversal CTE first, then vector similarity over reachable set.
    PreFilterCTE,
    /// Two-phase temporary table: populate candidate nodes into TEMP table, join with vector index.
    TwoPhaseTempTable,
}

/// Byte-budget cost model estimator for vector filter strategies (§5.3).
#[derive(Debug, Clone)]
pub struct CostEstimator {
    pub byte_budget: usize,
}

impl CostEstimator {
    pub fn new(byte_budget: usize) -> Self {
        Self { byte_budget }
    }

    /// Select optimal vector-filter strategy based on estimated candidate count.
    pub fn select_strategy(&self, candidate_count: usize) -> VectorFilterStrategy {
        if candidate_count < 500 {
            VectorFilterStrategy::PostFilter
        } else if candidate_count < 5000 {
            VectorFilterStrategy::PreFilterCTE
        } else {
            VectorFilterStrategy::TwoPhaseTempTable
        }
    }
}
