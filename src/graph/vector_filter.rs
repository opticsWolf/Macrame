//! Filtered vector search: strategies, the byte-budget cost model, and the
//! planner that chooses between them (§5.3, D-007).
//!
//! A vector query rarely arrives naked. The caller wants the ten nearest
//! neighbours of an embedding *among concepts reachable in two hops*, and the
//! two access paths cannot be nested: the DiskANN index is opaque to SQL
//! predicates, and the relational filter is opaque to the index. Composing them
//! is a cost decision, and this module makes it arithmetically rather than by
//! rule of thumb.
//!
//! # What changed in this cycle, and why the third strategy is gone
//!
//! §5.3 specified three strategies. `TwoPhaseTempTable` was to push the
//! candidate id set into the vector query as an allow-list, staging it through a
//! TEMP table. **Both of its mechanisms are absent from libSQL 0.9.30**, and
//! both were measured rather than reasoned about:
//!
//! * `CREATE TEMP TABLE` on the read connection fails with `SQLITE_READONLY
//!   (8)`. `PRAGMA query_only = ON` (D-019) covers the TEMP database too, and
//!   D-019 is the runtime half of the write-serialization guarantee, so it is
//!   the strategy that gives way, not the pragma.
//! * There is no allow-list to push into. `vector_top_k` refuses a fourth
//!   argument at runtime — *"too many arguments on vector_top_k() - max 3"* —
//!   and `vectorIndexSearch` in the bundled amalgamation rejects `argc != 3`
//!   before it looks at anything else.
//!
//! So the variant named an access path this engine does not offer, and the cost
//! table priced an operation that cannot be issued. It is removed rather than
//! kept as decoration: that is the precedent D-039 set with `louvain_communities`
//! returning one community per node. If a future libSQL gains a constrained
//! index walk, the variant comes back with a body, which is a smaller change
//! than the confusion of shipping a name with nothing behind it.
//!
//! # Why the strategy can never change the answer
//!
//! `PostFilter` retrieves a generous `k′` from the index and then discards
//! whatever fails the predicate. When the filter is tight the answer set falls
//! off the end of `k′`, and the classic implementation returns four rows for a
//! top-ten query without saying so. That is a silent wrong answer, which
//! Doctrine II exists to prevent, so it is not merely documented here — it is
//! detected. When a post-filtered pass comes back short *and* the underlying
//! index scan was saturated, the planner cannot conclude the matches do not
//! exist, so it escalates to the exact strategy and says so at `debug`.
//!
//! That gives the module its acceptance gate: the two strategies must agree, on
//! every query, for every graph. Strategy is then a performance decision and
//! nothing else, which is the only form in which a planner is safe.

use crate::error::{DbError, Result};
use crate::graph::builder::TraversalBuilder;
use crate::vector::{declared_dimension, ModelName, VectorSearchResult};

/// Strategy for combining vector search and graph traversal filters (§5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VectorFilterStrategy {
    /// Vector top-k′ from the index first, then discard what fails the filter.
    ///
    /// Cheap when the filter is loose, because most of the k′ survives. Degrades
    /// when it is tight, and [`FilteredVectorSearch`] escalates rather than
    /// under-returning when it detects that it has.
    PostFilter,
    /// Candidate ids from the traversal first, then exact distances over just
    /// those rows.
    ///
    /// Exact by construction — every candidate is scored, so nothing can fall
    /// off the end of a k′. A brute-force scan over `F32_BLOB` with no index,
    /// so it is priced by the candidate count.
    PreFilterCTE,
}

/// What the counting probe learned about the candidate set.
///
/// The distinction is the point: SQLite has no histograms and `sqlite_stat1`
/// carries average rows-per-key, which estimates an equality predicate and not
/// multi-hop reachability. So the count is *measured*, by running the traversal
/// under a cap — and a probe that hits its cap has not measured anything except
/// that the set is too big to care about the exact size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateCount {
    /// The traversal returned this many ids, below the cap.
    Exact(usize),
    /// The probe hit its cap. The true count is at least this.
    AtLeast(usize),
}

impl CandidateCount {
    /// The number to compute with. For a capped probe this understates the true
    /// count, which is the safe direction: it makes `PreFilterCTE` look cheaper
    /// than it is, and `PreFilterCTE` is the exact strategy.
    pub fn lower_bound(self) -> usize {
        match self {
            Self::Exact(n) | Self::AtLeast(n) => n,
        }
    }

    pub fn is_capped(self) -> bool {
        matches!(self, Self::AtLeast(_))
    }
}

/// One planning decision, with the arithmetic that produced it (D-007).
///
/// Returned rather than only logged, so a caller — and more importantly a test —
/// can assert on the choice instead of scraping `tracing` output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CostEstimate {
    pub strategy: VectorFilterStrategy,
    pub candidates: CandidateCount,
    /// Bytes `PostFilter` is estimated to touch.
    pub post_filter_bytes: usize,
    /// Bytes `PreFilterCTE` is estimated to touch.
    pub pre_filter_bytes: usize,
    /// The inflated k′ `PostFilter` would request.
    pub k_prime: usize,
}

/// Bytes of bookkeeping per candidate id carried through a filter pass.
///
/// A ULID is 26 characters and the `String` header is the rest. Deliberately an
/// estimate of the payload, matching how `Subgraph::estimated_bytes` accounts
/// for its own (D-047) — the two must use the same arithmetic or the budget
/// means different things in different modules.
const ID_BYTES: usize = 26 + std::mem::size_of::<String>();

/// Bytes of a result row once materialized: the id plus the score.
const ROW_BYTES: usize = ID_BYTES + std::mem::size_of::<f32>();

/// Byte-budget cost model estimator for vector filter strategies (§5.3, D-007).
///
/// The 0.4.5–0.5.4 version of this type carried a `byte_budget` field it never
/// read and branched on `candidate_count` against two hard-coded thresholds
/// (500, 5000) — D-007's interface with none of D-007's mechanism. It now prices
/// both strategies in bytes and takes the minimum, and `byte_budget` is a hard
/// ceiling on the candidate set rather than an unused field.
#[derive(Debug, Clone)]
pub struct CostEstimator {
    pub byte_budget: usize,
    /// Corpus size: how many vectors the model holds. Sets selectivity, and so
    /// the k′ inflation.
    pub corpus: usize,
    /// Bytes per stored vector, from the model's declared dimension (D-037).
    pub vector_bytes: usize,
}

impl CostEstimator {
    pub fn new(byte_budget: usize, corpus: usize, vector_bytes: usize) -> Self {
        Self {
            byte_budget,
            corpus,
            vector_bytes,
        }
    }

    /// The k′ `PostFilter` must request to expect `k` survivors.
    ///
    /// Selectivity is `candidates / corpus`, so `k′ = k × corpus / candidates`.
    /// Clamped to the corpus: asking the index for more rows than exist is not
    /// an error but it is not an estimate either, and letting it run away makes
    /// the cost comparison meaningless for tight filters — which is precisely
    /// when the comparison matters.
    pub fn k_prime(&self, k: usize, candidates: usize) -> usize {
        if candidates == 0 || self.corpus == 0 {
            return k;
        }
        let inflated = (k as u128 * self.corpus as u128) / candidates as u128;
        (inflated.max(k as u128) as usize).min(self.corpus.max(k))
    }

    /// Price both strategies and take the minimum (§5.3, D-007).
    ///
    /// | Strategy | Estimated bytes |
    /// |---|---|
    /// | `PostFilter` | `k′ × (vector_bytes + row_bytes)` + the filter pass |
    /// | `PreFilterCTE` | the filtered scan + `candidates × vector_bytes` |
    ///
    /// The filter pass is common to both — the traversal has to run either way —
    /// so it appears in both rows and cancels out of the comparison. It is
    /// included anyway, because the budget ceiling is checked against an absolute
    /// figure and a cost model that omits a term it "knows" cancels is a cost
    /// model that lies the moment someone adds a third strategy.
    pub fn estimate(&self, k: usize, candidates: CandidateCount) -> Result<CostEstimate> {
        let n = candidates.lower_bound();

        // The hard ceiling of §5.4, applied to the candidate set regardless of
        // strategy. A capped probe means the set is larger than this figure, so
        // refusing on the lower bound is the conservative direction.
        let candidate_bytes = n.saturating_mul(ID_BYTES);
        if candidate_bytes > self.byte_budget {
            return Err(DbError::SubgraphTooLarge {
                n: candidate_bytes,
                budget: self.byte_budget,
            });
        }

        let filter_pass = candidate_bytes;
        let k_prime = self.k_prime(k, n);

        let post_filter_bytes = k_prime
            .saturating_mul(self.vector_bytes.saturating_add(ROW_BYTES))
            .saturating_add(filter_pass);
        let pre_filter_bytes = n
            .saturating_mul(self.vector_bytes.saturating_add(ROW_BYTES))
            .saturating_add(filter_pass);

        let strategy = if post_filter_bytes <= pre_filter_bytes {
            VectorFilterStrategy::PostFilter
        } else {
            VectorFilterStrategy::PreFilterCTE
        };

        Ok(CostEstimate {
            strategy,
            candidates,
            post_filter_bytes,
            pre_filter_bytes,
            k_prime,
        })
    }
}

/// Default ceiling on the candidate set, in bytes.
pub const DEFAULT_BYTE_BUDGET: usize = 64 * 1024 * 1024;

/// Default cap on the counting probe.
///
/// The probe costs a fraction of what it prices, and the cap is what bounds
/// that fraction. Above it the planner knows only "more than the cap", which is
/// already enough to reject `PreFilterCTE`.
pub const DEFAULT_PROBE_CAP: usize = 10_000;

/// A vector search restricted to the nodes a traversal reaches (§5.3).
///
/// Mirrors [`TraversalBuilder`], which is the crate's established shape for a
/// read with options. The strategy is chosen by the planner and not by the
/// caller: D-007's whole content is that the choice is arithmetic, and a
/// parameter that lets a caller get it wrong would be a fidelity leak of the
/// kind Doctrine VIII names. [`Self::strategy`] exists to force a strategy in
/// tests — above all in the test that requires the two to agree.
#[derive(Debug, Clone)]
pub struct FilteredVectorSearch {
    model: ModelName,
    query: Vec<f32>,
    top_k: usize,
    traversal: TraversalBuilder,
    forced: Option<VectorFilterStrategy>,
    byte_budget: usize,
    probe_cap: usize,
}

impl FilteredVectorSearch {
    pub fn new(model: ModelName, query: Vec<f32>, traversal: TraversalBuilder) -> Self {
        Self {
            model,
            query,
            top_k: 10,
            traversal,
            forced: None,
            byte_budget: DEFAULT_BYTE_BUDGET,
            probe_cap: DEFAULT_PROBE_CAP,
        }
    }

    pub fn top_k(mut self, k: usize) -> Self {
        self.top_k = k;
        self
    }

    pub fn byte_budget(mut self, budget: usize) -> Self {
        self.byte_budget = budget;
        self
    }

    pub fn probe_cap(mut self, cap: usize) -> Self {
        self.probe_cap = cap;
        self
    }

    /// Force a strategy, bypassing the planner. For tests and diagnosis.
    pub fn strategy(mut self, strategy: VectorFilterStrategy) -> Self {
        self.forced = Some(strategy);
        self
    }

    /// Run the search, returning results and the plan that produced them.
    ///
    /// The estimate comes back so a caller can log it against reality, which is
    /// D-007's empirical-tuning requirement. `execute` is the same call with the
    /// plan dropped.
    pub async fn execute_explained(
        &self,
        conn: &libsql::Connection,
        now_ts: &str,
    ) -> Result<(Vec<VectorSearchResult>, CostEstimate)> {
        if self.top_k == 0 {
            let estimate = CostEstimate {
                strategy: VectorFilterStrategy::PreFilterCTE,
                candidates: CandidateCount::Exact(0),
                post_filter_bytes: 0,
                pre_filter_bytes: 0,
                k_prime: 0,
            };
            return Ok((Vec::new(), estimate));
        }

        // The probe is the traversal, run under a cap. It doubles as the
        // candidate set: having paid for the walk, throwing the ids away and
        // walking again for `PreFilterCTE` would be the cost model charging
        // twice for what it priced once.
        let (candidates, count) = self.probe(conn, now_ts).await?;

        let dim = declared_dimension(conn, &self.model).await?;
        let corpus = self.corpus_size(conn).await?;
        let estimator = CostEstimator::new(self.byte_budget, corpus, dim * 4);
        let mut estimate = estimator.estimate(self.top_k, count)?;

        if let Some(forced) = self.forced {
            estimate.strategy = forced;
        }

        tracing::debug!(
            strategy = ?estimate.strategy,
            candidates = candidates.len(),
            capped = count.is_capped(),
            k_prime = estimate.k_prime,
            post_filter_bytes = estimate.post_filter_bytes,
            pre_filter_bytes = estimate.pre_filter_bytes,
            "filtered vector search plan"
        );

        if candidates.is_empty() {
            return Ok((Vec::new(), estimate));
        }

        let results = match estimate.strategy {
            VectorFilterStrategy::PreFilterCTE => self.run_pre_filter(conn, &candidates).await?,
            VectorFilterStrategy::PostFilter => {
                let (rows, saturated) = self
                    .run_post_filter(conn, &candidates, estimate.k_prime)
                    .await?;
                // Short *and* saturated means the index scan ran out before the
                // filter did, so the missing rows may exist and may be nearer
                // than what came back. Escalate rather than under-return.
                if rows.len() < self.top_k && saturated {
                    tracing::debug!(
                        got = rows.len(),
                        want = self.top_k,
                        k_prime = estimate.k_prime,
                        "post-filter saturated and short; escalating to PreFilterCTE"
                    );
                    self.run_pre_filter(conn, &candidates).await?
                } else {
                    rows
                }
            }
        };

        Ok((results, estimate))
    }

    /// Run the search (§5.3).
    pub async fn execute(
        &self,
        conn: &libsql::Connection,
        now_ts: &str,
    ) -> Result<Vec<VectorSearchResult>> {
        Ok(self.execute_explained(conn, now_ts).await?.0)
    }

    /// The counting probe: the traversal, capped.
    async fn probe(
        &self,
        conn: &libsql::Connection,
        now_ts: &str,
    ) -> Result<(Vec<String>, CandidateCount)> {
        let mut ids = self.traversal.execute_ids(conn, now_ts).await?;
        let count = if ids.len() > self.probe_cap {
            ids.truncate(self.probe_cap);
            CandidateCount::AtLeast(self.probe_cap)
        } else {
            CandidateCount::Exact(ids.len())
        };
        Ok((ids, count))
    }

    /// How many vectors the model holds.
    ///
    /// **`COUNT(*)` per query, deliberately, and measured before being left
    /// alone (defect AF, Wave 3).** The objection was that D-007 argues strategy
    /// choice should be arithmetic rather than a rule of thumb, and the
    /// arithmetic's own input is O(corpus) while the thing it selects is not.
    /// True in mechanism. Measured:
    ///
    /// ```text
    /// corpus     2,000 vectors    5.2 µs
    /// corpus    20,000 vectors    8.5 µs
    /// whole filtered search       2.5 ms
    /// ```
    ///
    /// Ten times the corpus costs 1.6 times the time, because ~4.9 µs of it is a
    /// round trip and statement preparation — `declared_dimension`, which reads
    /// one `PRAGMA`, costs 5.0 µs flat for the same reason. Extrapolated to §9's
    /// stated 100K corpus that is ~22 µs against a 2.5 ms search: **under 1%.**
    ///
    /// So it is not cached, and the reason is worth stating because the
    /// implementation plan proposed caching it on the grounds that "neither
    /// `corpus_size` nor `declared_dimension` can change without DDL". That is
    /// true of the dimension and **false of the count** — it changes on every
    /// `upsert_embeddings`. Caching it would trade a real staleness bug for less
    /// than one percent of a query. `declared_dimension` *is* DDL-fixed and
    /// could be cached soundly; at 5 µs there is nothing to buy.
    async fn corpus_size(&self, conn: &libsql::Connection) -> Result<usize> {
        let sql = format!("SELECT COUNT(*) FROM {}", self.model.table());
        let n: i64 = conn
            .query(&sql, ())
            .await?
            .next()
            .await?
            .ok_or_else(|| DbError::ModelNotRegistered {
                model: self.model.to_string(),
                table: self.model.table(),
            })?
            .get(0)?;
        Ok(n as usize)
    }

    /// Exact distances over the candidate rows, ordered, limited to `top_k`.
    ///
    /// **It joins `concepts` for the same reason `search_vector` does** (0.13.18,
    /// W9.3, [D-191](../../docs/architecture/s13-decision-register.md#d-191)).
    /// This is the *third* reader of an embedding table, and the plan that
    /// closed F-31 named two — the argument for one predicate applied where the
    /// join is holds here exactly, and without it the two strategies would
    /// disagree about a retired concept, which is worse than both being wrong.
    /// `the_strategy_never_changes_the_answer` is the gate that says so.
    ///
    /// No `k'` inflation is needed on this path: the filter and the `LIMIT` are
    /// in one statement, so the limit already applies to survivors.
    ///
    /// **The instant comes from the traversal** (0.13.19, W9.4,
    /// [D-192](../../docs/architecture/s13-decision-register.md#d-192)), and it
    /// binds after the candidate chunk because the chunk is variadic and the
    /// instant is not.
    ///
    /// Candidate ids are carried in the statement as bound parameters, never
    /// spliced. A TEMP table would be the natural staging and is unavailable
    /// under `PRAGMA query_only` (measured: `SQLITE_READONLY (8)`); a bound
    /// placeholder list needs no write privilege at all, which makes the
    /// reformulation strictly better than the mechanism it replaces rather than
    /// a concession to it.
    async fn run_pre_filter(
        &self,
        conn: &libsql::Connection,
        candidates: &[String],
    ) -> Result<Vec<VectorSearchResult>> {
        let blob = self.encoded_query(conn).await?;
        let at = self.traversal.as_of_valid.as_deref();
        let mut out: Vec<VectorSearchResult> = Vec::new();

        // SQLITE_MAX_VARIABLE_NUMBER bounds one statement's parameter count, so
        // a candidate set larger than a chunk becomes several statements whose
        // results are merged. The alternative — one statement with the ids
        // interpolated — is the injection shape D-039 removed from the traversal
        // CTE, and it is not reintroduced here for a read path either.
        const IDS_PER_STATEMENT: usize = 500;
        for chunk in candidates.chunks(IDS_PER_STATEMENT) {
            let placeholders: Vec<String> =
                (0..chunk.len()).map(|i| format!("?{}", i + 2)).collect();
            let sql = format!(
                "SELECT e.concept_id, vector_distance_cos(e.embedding, ?1)
                   FROM {table} AS e
                   JOIN concepts AS c ON c.id = e.concept_id
                  WHERE e.concept_id IN ({ids})
                    AND {visible}
                  ORDER BY 2 ASC
                  LIMIT {k}",
                table = self.model.table(),
                ids = placeholders.join(", "),
                visible = crate::vector::search::visible_concept(at.map(|_| chunk.len() + 2)),
                k = self.top_k,
            );

            let mut params: Vec<libsql::Value> = vec![blob.clone().into()];
            params.extend(chunk.iter().map(|id| id.as_str().into()));
            if let Some(t) = at {
                params.push(t.into());
            }

            let mut rows = conn.query(&sql, params).await?;
            while let Some(row) = rows.next().await? {
                out.push(VectorSearchResult {
                    concept_id: row.get(0)?,
                    score: row.get::<f64>(1)? as f32,
                });
            }
        }

        // Each chunk was ordered and limited independently, so the merge has to
        // reorder. Total ordering by score with the id as tie-break: two rows at
        // an identical distance must not swap between runs, or the same query
        // answers differently on two machines.
        out.sort_by(|a, b| {
            a.score
                .partial_cmp(&b.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.concept_id.cmp(&b.concept_id))
        });
        out.truncate(self.top_k);
        Ok(out)
    }

    /// Top-k′ from the index, then discard what the filter rejects.
    ///
    /// Returns the survivors and whether the index scan was *saturated* — it
    /// returned every row it was asked for, so there were more it did not
    /// return. Saturation is what makes a short result inconclusive.
    ///
    /// It passes the traversal's instant down for the same reason it passes the
    /// model down: this arm's answer has to be the other arm's answer, and the
    /// gate that says so is `a_validity_that_ended_is_invisible_to_the_
    /// strategy_choice`.
    ///
    /// **It passes no half-life, and that is a decision** (0.13.20, W9.5,
    /// [D-193](../../docs/architecture/s13-decision-register.md#d-193)). Decay
    /// reorders whatever pool it is handed, and these two strategies do not
    /// hold the same pool: this one gets the k' the cost model priced, while
    /// `run_pre_filter` scores every candidate the traversal returned. Ranking
    /// by age inside each would make the answer a function of the byte estimate
    /// — which is the one thing [D-050](../../docs/architecture/s13-decision-register.md#d-050)
    /// forbids, and the property that makes having a planner safe at all.
    async fn run_post_filter(
        &self,
        conn: &libsql::Connection,
        candidates: &[String],
        k_prime: usize,
    ) -> Result<(Vec<VectorSearchResult>, bool)> {
        let hits = crate::vector::search_vector(
            conn,
            &self.query,
            &self.model,
            k_prime,
            self.traversal.as_of_valid.as_deref(),
            None,
        )
        .await?;
        let saturated = hits.len() >= k_prime;

        let allow: std::collections::HashSet<&str> =
            candidates.iter().map(String::as_str).collect();
        let mut out: Vec<VectorSearchResult> = hits
            .into_iter()
            .filter(|h| allow.contains(h.concept_id.as_str()))
            .collect();
        out.truncate(self.top_k);
        Ok((out, saturated))
    }

    async fn encoded_query(&self, conn: &libsql::Connection) -> Result<Vec<u8>> {
        let dim = declared_dimension(conn, &self.model).await?;
        crate::vector::EmbeddingCodec::encode(&self.query, dim, self.model.as_str())
    }
}
