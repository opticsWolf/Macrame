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

use std::time::Duration;

use crate::error::{DbError, Result};
use crate::vector::search::{decay_factor, rerank_depth};
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
#[non_exhaustive]
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
///
/// **The visibility predicate is the vector arm's, spliced rather than
/// repeated** (0.13.19, W9.4,
/// [D-192](../../docs/architecture/s13-decision-register.md#d-192)). This
/// function carried its own `AND c.retired = 0` from the day it was written,
/// and W9.3 wrote the shared constant without folding this copy into it. Two
/// literals that must agree is [D-030](../../docs/architecture/s13-decision-register.md#d-030)'s
/// failure class, and W9.4 is the release that would have made them disagree:
/// adding the valid-time bound to one and not the other is F-31 again with a
/// different column.
///
/// `as_of_valid` bounds each hit against its own valid interval. Absent, the
/// statement is what 0.13.18 issued. FTS5 is not consulted about it either way:
/// the MATCH selects on text and the bound is applied to the joined `concepts`
/// row, which is the only place either fact lives.
///
/// The join names `c.rowid_pk` rather than `c.rowid` (v8, D-119). They are the
/// same value — an `INTEGER PRIMARY KEY` *is* the rowid — but `concepts_fts`
/// declares `content_rowid='rowid_pk'`, and the join should say which key it is
/// joining on rather than rely on the alias holding.
pub async fn keyword_search(
    conn: &libsql::Connection,
    query: &str,
    top_k: usize,
    as_of_valid: Option<&str>,
    half_life: Option<Duration>,
) -> Result<Vec<(String, f64)>> {
    if top_k == 0 || query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let reference = match (half_life, as_of_valid) {
        (Some(_), None) => return Err(DbError::HalfLifeWithoutInstant),
        (Some(_), Some(t)) => Some(t),
        (None, _) => None,
    };

    // Deeper than the answer when the answer is about to be reordered, for the
    // reason `rerank_depth` states.
    let want = match half_life {
        Some(_) => rerank_depth(top_k),
        None => top_k,
    };
    let age_column = if half_life.is_some() {
        ", c.valid_from"
    } else {
        ""
    };

    let sql = format!(
        "SELECT c.id, bm25(concepts_fts) AS rank{age_column}
           FROM concepts_fts
           JOIN concepts c ON c.rowid_pk = concepts_fts.rowid
          WHERE concepts_fts MATCH ?1
            AND {visible}
          ORDER BY rank ASC, c.id ASC
          LIMIT ?2",
        visible = crate::vector::search::visible_concept(as_of_valid.map(|_| 3)),
    );

    let mut params: Vec<libsql::Value> = vec![query.into(), (want as i64).into()];
    if let Some(t) = as_of_valid {
        params.push(t.into());
    }
    let mut rows = conn.query(&sql, params).await?;
    let mut out: Vec<(String, f64)> = Vec::new();
    while let Some(row) = rows.next().await? {
        let id: String = row.get(0)?;
        let rank: f64 = row.get(1)?;
        let rank = match (reference, half_life) {
            (Some(reference), Some(half_life)) => {
                let valid_from: String = row.get(2)?;
                decayed_rank(rank, decay_factor(reference, &valid_from, half_life)?)
            }
            _ => rank,
        };
        out.push((id, rank));
    }

    if half_life.is_some() {
        out.sort_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        out.truncate(top_k);
    }
    Ok(out)
}

/// A bm25 rank, decayed, still a bm25-shaped rank (0.13.20, W9.5, D-193).
///
/// **This is where the two surfaces stop being the same operation.**
/// [`crate::vector::search::decayed_distance`] has to convert, because a
/// distance multiplied by a factor in (0, 1] gets *smaller* and a smaller
/// distance is a better hit. Here the plain multiply is already right, and for
/// a reason worth stating rather than relying on: bm25 arrives **negative**,
/// with magnitude growing in relevance, so it is a negated similarity already.
/// Multiplying moves a hit toward zero, and toward zero is toward the far end
/// of an ascending best-first list — exactly the demotion decay is for.
///
/// So the operation that would have been the bug on the vector surface is the
/// correct one here, and writing them as one shared helper would have made one
/// of the two wrong. `a_half_life_ranks_by_age_in_every_arm` asserts both
/// orders, which is what stops that from being a comment nobody re-checks.
///
/// A non-negative rank is left alone rather than multiplied. FTS5 does not
/// produce one on this path, and if it ever did, multiplying would move it
/// toward zero from the *other* side — an improvement, which is the one thing
/// decay must never be.
fn decayed_rank(rank: f64, factor: f64) -> f64 {
    if rank < 0.0 {
        rank * factor
    } else {
        rank
    }
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
    as_of_valid: Option<String>,
    half_life: Option<Duration>,
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
            as_of_valid: None,
            half_life: None,
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

    /// Read both arms at a valid-time instant (0.13.19, W9.4, F-32).
    ///
    /// **Both, and it could not be one.** RRF fuses two rank lists, so an
    /// instant applied to one arm and not the other would fuse what was true
    /// then with what is true now and return a single ranked list that is
    /// neither — the fused score cannot say which arm the anachronism came
    /// from, which is the property that makes a half-applied bound worse here
    /// than on either arm alone.
    ///
    /// Named for [`crate::graph::TraversalBuilder::as_of_valid`], and it is the
    /// same axis: *what was true*, bounded by the concept's own interval.
    /// Absent, both arms read the corpus, unchanged.
    pub fn as_of_valid(mut self, ts: impl Into<String>) -> Self {
        self.as_of_valid = Some(ts.into());
        self
    }

    /// Weight each arm's ranking by the age of what it matched (0.13.20, W9.5).
    ///
    /// **Both arms, and before the fusion rather than after it.** RRF adds
    /// *ranks*; a decay applied to the fused score afterwards would be
    /// penalising a number that is already scale-free and would leave both
    /// arms' orderings — the only thing RRF reads — untouched. So each arm
    /// decays its own similarity and re-sorts, and the fusion sees two lists
    /// that already price age.
    ///
    /// Requires [`Self::as_of_valid`]: age is measured from the instant the
    /// search reads at, and there is no other instant here to fall back to. The
    /// arms raise [`DbError::HalfLifeWithoutInstant`] rather than defaulting to
    /// now.
    pub fn half_life(mut self, half_life: Duration) -> Self {
        self.half_life = Some(half_life);
        self
    }

    fn effective_depth(&self) -> usize {
        self.depth.unwrap_or_else(|| rerank_depth(self.top_k))
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
        let at = self.as_of_valid.as_deref();
        let vector: Vec<VectorSearchResult> = search_vector(
            conn,
            &self.query_vector,
            &self.model,
            depth,
            at,
            self.half_life,
        )
        .await?;

        let match_expr = if self.raw_match {
            self.query_text.clone()
        } else {
            escape_fts5_query(&self.query_text)
        };
        let keyword = keyword_search(conn, &match_expr, depth, at, self.half_life).await?;

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
