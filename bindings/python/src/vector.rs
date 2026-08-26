//! The vector surface (P4.4): embeddings, search, and the filtered planner.
//!
//! # A model is a name, and the name is validated once
//!
//! [`macrame::vector::ModelName`] exists because a model name reaches SQL as an
//! **identifier** — `ModelName::table()` and `::index()` build
//! `embeddings_<name>` and its DiskANN index — so it is the one string in this
//! crate that cannot be bound as a parameter. Every method here takes a plain
//! `str` and constructs the `ModelName` at the boundary, so an invalid name is
//! `InvalidModelNameError` from the call that used it rather than a SQL syntax
//! error from somewhere underneath.
//!
//! # Results are classes with two fields, which contradicts [`crate::temporal`]
//!
//! Edge tuples stayed tuples there; `VectorHit` is a class here with
//! `concept_id` and `score`. The difference is what the second field means. An
//! edge tuple's five slots are all identifiers and instants and a caller reading
//! `e[3]` is reading a timestamp either way. A search result's `score` is a
//! **cosine distance** from `search_vector` (smaller is closer), a **fused RRF
//! score** from `hybrid_search` (larger is better), and a *bm25* rank from
//! `keyword_search` (negative, and ascending is best-first). Three incompatible
//! orderings behind one position. `hit[1]` invites sorting the wrong way; a
//! named field with a docstring does not stop it, but it gives the caller
//! somewhere to look.
//!
//! `keyword_search` keeps its `(id, rank)` tuples for that reason inverted: it
//! is the one arm whose ordering is fixed by FTS5 and documented at the call.

use pyo3::prelude::*;

use macrame::graph::{CandidateCount, CostEstimate, VectorFilterStrategy};
use macrame::vector::{HybridHit, ModelName, VectorSearchResult};

use crate::errors::to_py;

/// `str` → `ModelName`, refused at the boundary rather than in SQL.
pub(crate) fn model_name(name: &str) -> PyResult<ModelName> {
    ModelName::new(name).map_err(to_py)
}

/// One hit from a vector or hybrid search.
///
/// **`score` means different things by search.** Cosine *distance* from
/// `search_vector` and `search_filtered` — smaller is closer, and the list is
/// already ascending. A fused reciprocal-rank score from `hybrid_search` —
/// larger is better, and that list is already descending. Both arrive sorted;
/// re-sorting either needs the right direction.
#[pyclass(name = "VectorHit", module = "macrame", frozen)]
pub(crate) struct PyVectorHit {
    concept_id: String,
    score: f64,
    vector_rank: Option<usize>,
    keyword_rank: Option<usize>,
}

#[pymethods]
impl PyVectorHit {
    #[getter]
    fn concept_id(&self) -> &str {
        &self.concept_id
    }
    /// See the class docstring: the direction depends on which search produced
    /// this.
    #[getter]
    fn score(&self) -> f64 {
        self.score
    }
    /// This concept's rank in the vector arm, or `None` if that arm missed it.
    ///
    /// Only a hybrid search fills these. `None` in one of them is the useful
    /// signal: a hit both arms found is a different kind of hit from one only
    /// the keyword arm found, and the fused score alone cannot say which it is.
    #[getter]
    fn vector_rank(&self) -> Option<usize> {
        self.vector_rank
    }
    /// This concept's rank in the keyword arm, or `None` if that arm missed it.
    #[getter]
    fn keyword_rank(&self) -> Option<usize> {
        self.keyword_rank
    }
    fn __repr__(&self) -> String {
        format!(
            "<macrame.VectorHit {:?} score={}>",
            self.concept_id, self.score
        )
    }
}

impl From<VectorSearchResult> for PyVectorHit {
    fn from(r: VectorSearchResult) -> Self {
        Self {
            concept_id: r.concept_id,
            // Widened from f32. The stored vectors are f32 and the distance is
            // computed there; this is a lossless widening, not added precision.
            score: r.score as f64,
            vector_rank: None,
            keyword_rank: None,
        }
    }
}

impl From<HybridHit> for PyVectorHit {
    fn from(h: HybridHit) -> Self {
        Self {
            concept_id: h.concept_id,
            score: h.score,
            vector_rank: h.vector_rank,
            keyword_rank: h.keyword_rank,
        }
    }
}

/// Which way `search_filtered` combined the vector index and the traversal.
#[pyclass(
    name = "FilterStrategy",
    module = "macrame",
    eq,
    eq_int,
    frozen,
    from_py_object
)]
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum PyFilterStrategy {
    /// Vector top-k′ from the index first, then discard what fails the filter.
    /// Cheap when the filter is loose.
    #[pyo3(name = "POST_FILTER")]
    PostFilter,
    /// Candidate ids from the traversal first, then exact distances over just
    /// those rows. Exact by construction, and priced by the candidate count.
    #[pyo3(name = "PRE_FILTER_CTE")]
    PreFilterCte,
}

impl From<PyFilterStrategy> for VectorFilterStrategy {
    fn from(s: PyFilterStrategy) -> Self {
        match s {
            PyFilterStrategy::PostFilter => VectorFilterStrategy::PostFilter,
            PyFilterStrategy::PreFilterCte => VectorFilterStrategy::PreFilterCTE,
        }
    }
}

impl From<VectorFilterStrategy> for PyFilterStrategy {
    fn from(s: VectorFilterStrategy) -> Self {
        match s {
            VectorFilterStrategy::PostFilter => PyFilterStrategy::PostFilter,
            VectorFilterStrategy::PreFilterCTE => PyFilterStrategy::PreFilterCte,
            // See `types.rs`'s arm for `AttributeMode`: mandatory under
            // `#[non_exhaustive]` (0.13.34, D-207), unreachable while
            // `binding_parity_tests` passes, and a panic because naming the
            // wrong strategy is a wrong answer rather than a missing one.
            other => unreachable!("unmapped VectorFilterStrategy: {other:?}"),
        }
    }
}

/// The planner's decision, with the arithmetic that produced it (D-007).
///
/// Returned rather than only logged, so a caller can compare the estimate
/// against what the query actually cost — which is what D-007's
/// empirical-tuning requirement asks for and what scraping `tracing` output
/// cannot support.
#[pyclass(name = "CostEstimate", module = "macrame", frozen)]
pub(crate) struct PyCostEstimate {
    inner: CostEstimate,
}

#[pymethods]
impl PyCostEstimate {
    #[getter]
    fn strategy(&self) -> PyFilterStrategy {
        self.inner.strategy.into()
    }
    /// How many candidates the probe found.
    ///
    /// **Read with `candidates_capped`.** SQLite has no histograms, so this is
    /// measured by running the traversal under a cap — and a probe that hit its
    /// cap has measured only that the set is bigger than the cap.
    #[getter]
    fn candidates(&self) -> usize {
        self.inner.candidates.lower_bound()
    }
    /// True when the probe hit its cap, so `candidates` is a lower bound.
    #[getter]
    fn candidates_capped(&self) -> bool {
        self.inner.candidates.is_capped()
    }
    #[getter]
    fn post_filter_bytes(&self) -> usize {
        self.inner.post_filter_bytes
    }
    #[getter]
    fn pre_filter_bytes(&self) -> usize {
        self.inner.pre_filter_bytes
    }
    /// The inflated k′ `POST_FILTER` would request from the index.
    #[getter]
    fn k_prime(&self) -> usize {
        self.inner.k_prime
    }
    fn __repr__(&self) -> String {
        format!(
            "<macrame.CostEstimate strategy={} candidates={}{}>",
            match self.inner.strategy {
                VectorFilterStrategy::PostFilter => "POST_FILTER",
                VectorFilterStrategy::PreFilterCTE => "PRE_FILTER_CTE",
                // A `__repr__` must not panic -- it is what a debugger and a
                // traceback call -- so this one arm degrades instead. The
                // strategy is still readable through the `strategy` getter,
                // which does panic, so nothing is silently hidden.
                _ => "UNKNOWN",
            },
            self.inner.candidates.lower_bound(),
            if self.inner.candidates.is_capped() {
                "+"
            } else {
                ""
            }
        )
    }
}

impl PyCostEstimate {
    pub(crate) fn new(inner: CostEstimate) -> Self {
        Self { inner }
    }
}

/// `CandidateCount` is not exposed as a type of its own.
///
/// It is a two-variant enum whose whole content is "a number, and whether it is
/// exact". That is two attributes on `CostEstimate` — `candidates` and
/// `candidates_capped` — and a class would make a caller unwrap it to reach the
/// number they wanted, which is how the cap ends up ignored.
const _: Option<CandidateCount> = None;

pub(crate) fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyVectorHit>()?;
    m.add_class::<PyFilterStrategy>()?;
    m.add_class::<PyCostEstimate>()?;
    // The RRF constant the hybrid fusion uses by default. Exposed because
    // `hybrid_search(rrf_k=…)` is tunable and a caller changing it wants to know
    // what they are changing it from.
    m.add("RRF_K", macrame::vector::RRF_K)?;
    Ok(())
}
