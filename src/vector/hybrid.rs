//! Hybrid search: the keyword arm, and its fusion with the vector arm (§5.9).
//!
//! Dense vectors and keyword matching fail in opposite directions. An embedding
//! finds a paraphrase and misses an exact identifier it never saw in training;
//! BM25 finds the identifier and misses the paraphrase entirely. Reciprocal Rank
//! Fusion combines them without either needing to know the other's score scale,
//! which is the property that makes it usable here: cosine distance and BM25 are
//! not comparable numbers, and any scheme that adds them is inventing a
//! conversion nobody measured. RRF adds *ranks*, which are comparable by
//! construction.
//!
//! Before this existed, `reciprocal_rank_fusion` was a pure function over two
//! rank lists with nothing in the crate producing the keyword half and no FTS5
//! table in the schema — §9 budgeted hybrid search at ≤50 ms for a path that
//! could not run. The fusion function is unchanged in substance; what is new is
//! everything that feeds it.

use crate::error::Result;
use crate::vector::{reciprocal_rank_fusion, search_vector, ModelName, VectorSearchResult};

/// The `k` in `1/(k + rank)`, from the paper and from §5.9.
///
/// It damps the contribution of top ranks so that agreement between the two arms
/// outweighs a single arm's confidence: at k = 60 the gap between rank 1 and
/// rank 2 is small, so a document both arms rank tenth beats one that is first in
/// one list and absent from the other. Lower it and the fusion approaches "best
/// of either arm"; raise it and it approaches "appears in both".
pub const RRF_K: usize = 60;

/// One fused result, with the evidence for its position.
///
/// The per-arm ranks are carried out rather than discarded because a fused score
/// alone is unreadable: `0.032` says nothing, while "rank 2 by vector, absent
/// from keyword" says exactly why a document placed where it did. This is the
/// same reasoning that makes `FilteredVectorSearch` return its `CostEstimate`.
#[derive(Debug, Clone, PartialEq)]
pub struct HybridHit {
    pub concept_id: String,
    /// Fused RRF score. Higher is better; the scale is not meaningful on its own.
    pub score: f64,
    /// 1-based rank in the vector arm, or `None` if that arm did not return it.
    pub vector_rank: Option<usize>,
    /// 1-based rank in the keyword arm, or `None`.
    pub keyword_rank: Option<usize>,
}

/// Turn arbitrary user text into an FTS5 MATCH expression that cannot be a
/// syntax error and cannot mean something the user did not write.
///
/// FTS5's match syntax is a language: `AND`, `OR`, `NOT`, `NEAR`, prefix `*`,
/// column filters like `title:`, and quoted phrases. Passing a raw search box
/// through to it has two failure modes, and neither is acceptable as a default.
/// A query containing an unbalanced quote or a bare `AND` raises
/// `SQLITE_ERROR` — the user typed a search and got an exception. And a query
/// containing `NOT` silently *means* something: searching for `cats not dogs`
/// quietly excludes documents, which is a wrong answer rather than an error.
///
/// So each run of alphanumeric characters becomes one double-quoted term and
/// everything else is dropped, leaving implicit AND between terms. A caller who
/// genuinely wants the query language can pass it through with
/// [`HybridSearch::raw_match`].
pub fn escape_fts5_query(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 8);
    for token in input.split(|c: char| !c.is_alphanumeric()) {
        if token.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push('"');
        out.push_str(token);
        out.push('"');
    }
    out
}

/// Keyword search over concept text, best match first (§5.9).
///
/// Ranked by `bm25`, which FTS5 returns as a *negative* number whose magnitude
/// grows with relevance, so ascending order is best-first. Retired concepts are
/// excluded: a soft-deleted concept is not a search result, and the index cannot
/// filter on `retired` itself because external-content FTS5 indexes only the
/// columns it was declared over.
pub async fn keyword_search(
    conn: &libsql::Connection,
    query: &str,
    top_k: usize,
) -> Result<Vec<(String, f64)>> {
    if top_k == 0 || query.trim().is_empty() {
        return Ok(Vec::new());
    }

    let sql = "SELECT c.id, bm25(concepts_fts) AS rank
                 FROM concepts_fts
                 JOIN concepts c ON c.rowid = concepts_fts.rowid
                WHERE concepts_fts MATCH ?1
                  AND c.retired = 0
                ORDER BY rank ASC, c.id ASC
                LIMIT ?2";

    let mut rows = conn
        .query(sql, libsql::params![query, top_k as i64])
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push((row.get(0)?, row.get::<f64>(1)?));
    }
    Ok(out)
}

/// A hybrid search over one model's vectors and the concept-text index (§5.9).
///
/// Mirrors [`crate::graph::FilteredVectorSearch`] and `TraversalBuilder`, which
/// is the crate's shape for a read with options.
#[derive(Debug, Clone)]
pub struct HybridSearch {
    model: ModelName,
    query_text: String,
    query_vector: Vec<f32>,
    top_k: usize,
    depth: Option<usize>,
    rrf_k: usize,
    raw_match: bool,
}

impl HybridSearch {
    /// `query_text` feeds the keyword arm, `query_vector` the vector arm. They
    /// are separate parameters because the crate does not embed text — that is
    /// the caller's model, run in the caller's process (Doctrine VII), and the
    /// two arms may legitimately be given different framings of one question.
    pub fn new(model: ModelName, query_text: impl Into<String>, query_vector: Vec<f32>) -> Self {
        Self {
            model,
            query_text: query_text.into(),
            query_vector,
            top_k: 10,
            depth: None,
            rrf_k: RRF_K,
            raw_match: false,
        }
    }

    pub fn top_k(mut self, k: usize) -> Self {
        self.top_k = k;
        self
    }

    /// How deep to read each arm before fusing. Defaults to `max(5 × top_k, 50)`.
    ///
    /// Fusing two top-`k` lists is not the same as the top `k` of the fusion: a
    /// document ranked 12th by both arms can outscore one ranked 1st by a single
    /// arm, and it is invisible if neither list was read past 10. Depth is what
    /// buys those, and it costs one larger `LIMIT` per arm rather than an extra
    /// round trip.
    pub fn depth(mut self, depth: usize) -> Self {
        self.depth = Some(depth);
        self
    }

    /// Override the RRF damping constant. See [`RRF_K`].
    pub fn rrf_k(mut self, k: usize) -> Self {
        self.rrf_k = k;
        self
    }

    /// Pass `query_text` to FTS5 verbatim instead of escaping it.
    ///
    /// Opt-in, because it hands the caller's string to a query language: a
    /// malformed expression becomes an engine error and `NOT` silently changes
    /// what was asked. Correct for a caller building the expression themselves;
    /// wrong for anything typed into a search box.
    pub fn raw_match(mut self, raw: bool) -> Self {
        self.raw_match = raw;
        self
    }

    fn effective_depth(&self) -> usize {
        self.depth.unwrap_or_else(|| (self.top_k * 5).max(50))
    }

    /// Run both arms and fuse them (§5.9).
    pub async fn execute(&self, conn: &libsql::Connection) -> Result<Vec<HybridHit>> {
        if self.top_k == 0 {
            return Ok(Vec::new());
        }
        let depth = self.effective_depth();

        // The vector arm. An unregistered model is a typed error from here, and
        // is deliberately not softened into "no vector results": a caller who
        // named a model that does not exist asked a question this cannot answer.
        let vector: Vec<VectorSearchResult> =
            search_vector(conn, &self.query_vector, &self.model, depth).await?;

        let match_expr = if self.raw_match {
            self.query_text.clone()
        } else {
            escape_fts5_query(&self.query_text)
        };
        let keyword = keyword_search(conn, &match_expr, depth).await?;

        let vector_ids: Vec<String> = vector.iter().map(|v| v.concept_id.clone()).collect();
        let keyword_ids: Vec<String> = keyword.iter().map(|(id, _)| id.clone()).collect();

        let fused = reciprocal_rank_fusion(&vector_ids, &keyword_ids, self.rrf_k);

        let rank_of = |list: &[String], id: &str| list.iter().position(|x| x == id).map(|i| i + 1);

        Ok(fused
            .into_iter()
            .take(self.top_k)
            .map(|(concept_id, score)| HybridHit {
                vector_rank: rank_of(&vector_ids, &concept_id),
                keyword_rank: rank_of(&keyword_ids, &concept_id),
                concept_id,
                score,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_turns_a_search_box_into_terms() {
        assert_eq!(
            escape_fts5_query("bitemporal ledger"),
            r#""bitemporal" "ledger""#
        );
        // The operators that would otherwise change the meaning of the query.
        assert_eq!(escape_fts5_query("cats NOT dogs"), r#""cats" "NOT" "dogs""#);
        // The syntax errors: an unbalanced quote, a trailing operator, a column
        // filter. None of these survive as syntax.
        assert_eq!(escape_fts5_query(r#"a" OR "b"#), r#""a" "OR" "b""#);
        assert_eq!(escape_fts5_query("title:macrame"), r#""title" "macrame""#);
        assert_eq!(escape_fts5_query("trailing AND"), r#""trailing" "AND""#);
    }

    /// A query of nothing but punctuation escapes to the empty string, which
    /// `keyword_search` must treat as "no keyword arm" rather than handing FTS5
    /// an empty MATCH — that is a syntax error, not an empty result.
    #[test]
    fn a_query_with_no_terms_escapes_to_nothing() {
        assert_eq!(escape_fts5_query("!!! ???"), "");
        assert_eq!(escape_fts5_query(""), "");
    }

    #[test]
    fn unicode_survives_escaping() {
        assert_eq!(escape_fts5_query("Müller größe"), r#""Müller" "größe""#);
    }
}
