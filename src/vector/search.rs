use std::collections::HashMap;

use crate::error::Result;
use crate::vector::registry::declared_dimension;
use crate::vector::{EmbeddingCodec, ModelName};

/// Search result container for vector similarity or hybrid search (§5.9).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VectorSearchResult {
    pub concept_id: String,
    /// Cosine distance: 0.0 is identical, larger is further. Ascending order.
    pub score: f32,
}

/// Store or replace a concept's vector for one model (§4.1, Doctrine VII).
///
/// An embedding is derived, so re-embedding the same concept under the same
/// model replaces the row rather than versioning it — the ledger records that
/// the concept changed, and the vector is recomputed from the concept. Nothing
/// here writes to `transaction_log`; there are no triggers on this table.
///
/// The dimension is checked against the model's *declared* dimension before the
/// statement is built, so the caller gets [`DimMismatch`] naming both numbers
/// rather than the engine's `dimensions are different: 2 != 4`.
///
/// # Prefer [`crate::Database::upsert_embeddings`]
///
/// This takes a **bare connection** and is therefore §4.7 invariant 2's third
/// hole: a write that does not cross the actor's channel. Hidden from the docs
/// alongside [`crate::Database::raw`] (D-091) so the documented path is the one
/// that preserves the single-writer property; still public, for the reason
/// [D-068](../../docs/architecture/s13-decision-register.md#d-068) gives.
///
/// [`DimMismatch`]: crate::error::DbError::DimMismatch
#[doc(hidden)]
pub async fn upsert_embedding(
    conn: &libsql::Connection,
    model: &ModelName,
    concept_id: &str,
    vector: &[f32],
) -> Result<()> {
    let blob = encode_for_model(conn, model, vector).await?;

    // `model.table()` is a bare identifier by construction; the values bind.
    conn.execute(
        &format!(
            "INSERT INTO {table} (concept_id, embedding) VALUES (?1, ?2)
             ON CONFLICT(concept_id) DO UPDATE SET embedding = excluded.embedding",
            table = model.table()
        ),
        libsql::params![concept_id, blob],
    )
    .await?;
    Ok(())
}

/// Store or replace one chunk of vectors for a model, in a single transaction.
///
/// The dimension is resolved **once per chunk**, not once per row.
/// [`declared_dimension`] is a `PRAGMA table_info` round trip, so resolving it
/// per row turns a bulk embed into one round trip per vector — and the answer
/// cannot change inside a chunk, because the chunk holds the write lock and the
/// dimension is a property of a table only `register_model` creates.
///
/// Atomic per chunk, not across chunks: a failure partway leaves earlier chunks
/// committed. That is the right trade here in a way it would not be for
/// assertions — an embedding is a derived artifact (Doctrine VII), so a
/// partially written batch is recoverable by re-embedding, whereas a partially
/// written history is not recoverable at all.
pub(crate) async fn upsert_embedding_chunk(
    conn: &libsql::Connection,
    model: &ModelName,
    rows: &[(String, Vec<f32>)],
) -> Result<usize> {
    if rows.is_empty() {
        return Ok(0);
    }

    let dim = declared_dimension(conn, model).await?;
    let sql = format!(
        "INSERT INTO {table} (concept_id, embedding) VALUES (?1, ?2)
         ON CONFLICT(concept_id) DO UPDATE SET embedding = excluded.embedding",
        table = model.table()
    );

    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;

    // Prepared once per chunk, for the same reason the dimension is resolved once
    // per chunk and the same reason the edge chunk hoists its insert (D-056): the
    // statement text is identical for every row, and the embedding tables carry a
    // DiskANN index whose maintenance is compiled into each preparation.
    //
    // `reset()` between rows is required — libsql's `Statement::execute` binds and
    // steps without resetting first.
    let stmt = tx.prepare(&sql).await?;

    let res: Result<()> = async {
        for (concept_id, vector) in rows {
            let blob = EmbeddingCodec::encode(vector, dim, model.as_str())?;
            stmt.reset();
            stmt.execute(libsql::params![concept_id.as_str(), blob])
                .await?;
        }
        Ok(())
    }
    .await;

    // Dropped before either arm ends the transaction: a live statement on the
    // connection is what makes SQLite refuse to commit or roll back.
    drop(stmt);

    match res {
        Ok(()) => {
            tx.commit().await?;
            Ok(rows.len())
        }
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e)
        }
    }
}

/// **The visibility predicate every vector read applies** (0.13.18, W9.3,
/// [D-191](../../docs/architecture/s13-decision-register.md#d-191)).
///
/// Written once and spliced, because the alternative is what produced F-31:
/// [`crate::vector::keyword_search`] carried `AND c.retired = 0` from the day it
/// was written and nothing propagated the obligation to the vector arm, so one
/// half of `hybrid_search` saw a retirement and the other did not.
///
/// It is bound to the alias **`c`**, and every query splicing it joins
/// `concepts AS c`. That join is an inner join and is not a second filter: the
/// embedding tables carry a foreign key to `concepts` ([§4.6](../../docs/architecture/s4-schema.md)),
/// so a vector with no concept behind it cannot exist and the join drops
/// nothing on its own.
pub(crate) const VISIBLE_CONCEPT: &str = "c.retired = 0";

/// [`VISIBLE_CONCEPT`], and the valid-time bound when the read states an instant
/// (0.13.19, W9.4, [D-192](../../docs/architecture/s13-decision-register.md#d-192)).
///
/// `at_param` is the **1-based statement parameter** the instant is bound to,
/// which differs per query and so cannot be baked into a constant: the vector
/// search has three parameters ahead of it, the keyword search two, and the
/// pre-filter's is a function of the candidate chunk. Passing the index rather
/// than the value keeps the instant a bound parameter on every path.
///
/// `None` yields the retirement predicate alone and nothing else changes, which
/// is [D-155](../../docs/architecture/s13-decision-register.md#d-155)'s rule:
/// an absent knob leaves the mechanism alone. The bound is the crate's
/// half-open interval — `valid_from <= t AND t < valid_to`, the same one
/// `hydrate_current` and the traversal CTE apply — so a row whose validity has
/// just ended at `t` is out and one whose validity begins at `t` is in.
pub(crate) fn visible_concept(at_param: Option<usize>) -> String {
    match at_param {
        None => VISIBLE_CONCEPT.to_string(),
        Some(p) => {
            format!("{VISIBLE_CONCEPT} AND c.valid_from <= ?{p} AND ?{p} < c.valid_to")
        }
    }
}

/// Top-k nearest **visible** neighbours for `query_vec` under `model` (§5.9).
///
/// Goes through `vector_top_k`, which consults the DiskANN index, rather than
/// scanning the table and sorting: the index is what §9's "top-10 over 100K
/// concepts in ≤20 ms" budget assumes, and an `ORDER BY vector_distance_cos(…)`
/// over the whole table is linear in the corpus no matter how small `k` is.
/// `vector_top_k` yields base-table rowids, so the distance is recomputed on the
/// k rows it selects — k distance evaluations, not one per concept.
///
/// # `top_k` is a count, and keeping it one is the whole of the loop
///
/// The index chooses its `k` rows before the visibility predicate can see them,
/// so a filter applied afterwards returns fewer than `k` whenever a retired
/// concept is among them. Letting `top_k` become a *ceiling* would be a silent
/// behaviour change for every existing caller, so the index is asked for a
/// larger `k'` instead — the escalation
/// [`crate::graph::FilteredVectorSearch`] already performs against the same
/// problem, and the inflation `CostEstimator::k_prime` computes from
/// selectivity.
///
/// It runs **only when the first pass comes up short**, which is the case where
/// something was actually filtered out. A corpus with nothing retired — the
/// overwhelmingly common one — pays one query and no count, which is why the
/// loop is here rather than a selectivity estimate computed up front: that
/// would put two `COUNT(*)`s on every search to serve the case that almost
/// never arises.
///
/// Termination is by exhaustion, not by a retry budget. `k'` doubles until the
/// index has been asked for the whole table, and a `k'` at or above the row
/// count means what came back **is** every visible neighbour — a complete
/// answer, not a truncated one. The row count is read at most once and only on
/// the escalating path.
///
/// # `as_of_valid`: what was true then, or the corpus (0.13.19, W9.4, F-32)
///
/// With an instant, a concept is a result only while its own valid interval
/// contains it — the half-open bound the whole crate uses, and the same clause
/// the visibility predicate already carries, so it costs no extra join. Without
/// one, the statement is byte-for-byte what 0.13.18 issued: an absent knob
/// leaves the mechanism alone
/// ([D-155](../../docs/architecture/s13-decision-register.md#d-155)).
///
/// It is `as_of_valid` and not `as_of` because
/// [`crate::graph::TraversalBuilder::as_of_valid`] split that word into two
/// axes in 0.13.2, and one spelling per axis is the point of having split it.
/// **Transaction time is deliberately not offered here**: reading the index as
/// it stood at a past `recorded_at` would mean searching vectors that have
/// since been replaced, and the DiskANN index holds one row per concept with no
/// history to search. A caller who wants that asks the ledger, not the index.
///
/// The escalation above needs no adjustment for it. It keys on a pass coming up
/// short and not on **why** it came up short, so a corpus thinned by valid time
/// re-asks the index exactly as one thinned by retirement does.
pub async fn search_vector(
    conn: &libsql::Connection,
    query_vec: &[f32],
    model: &ModelName,
    top_k: usize,
    as_of_valid: Option<&str>,
) -> Result<Vec<VectorSearchResult>> {
    if top_k == 0 {
        return Ok(Vec::new());
    }
    let blob = encode_for_model(conn, model, query_vec).await?;

    // `?2` is what the index is asked for and `?3` is what the caller asked
    // for; they are the same on the first pass and diverge on escalation.
    let sql = format!(
        "SELECT e.concept_id, vector_distance_cos(e.embedding, ?1)
           FROM vector_top_k('{index}', ?1, ?2) AS t
           JOIN {table} AS e ON e.rowid = t.id
           JOIN concepts AS c ON c.id = e.concept_id
          WHERE {visible}
          ORDER BY 2 ASC
          LIMIT ?3",
        index = model.index(),
        table = model.table(),
        visible = visible_concept(as_of_valid.map(|_| 4)),
    );

    let mut k_prime = top_k;
    let mut indexed: Option<usize> = None;

    loop {
        let mut params: Vec<libsql::Value> = vec![
            blob.clone().into(),
            (k_prime as i64).into(),
            (top_k as i64).into(),
        ];
        if let Some(t) = as_of_valid {
            params.push(t.into());
        }
        let mut rows = conn.query(&sql, params).await?;

        let mut results = Vec::new();
        while let Some(row) = rows.next().await? {
            results.push(VectorSearchResult {
                concept_id: row.get(0)?,
                // The distance is computed by the engine over a non-null
                // F32_BLOB column, so a null here would mean the schema is not
                // what we think.
                score: row.get::<f64>(1)? as f32,
            });
        }

        if results.len() >= top_k {
            return Ok(results);
        }

        let n = match indexed {
            Some(n) => n,
            None => {
                let n = indexed_rows(conn, model).await?;
                indexed = Some(n);
                n
            }
        };
        // The index has already been asked for everything it holds, so this is
        // every visible neighbour there is.
        if k_prime >= n {
            return Ok(results);
        }
        k_prime = k_prime.saturating_mul(2).min(n);
    }
}

/// How many vectors `model` holds — the ceiling `search_vector` escalates to.
///
/// Read lazily and at most once per search: see [`search_vector`] for why it is
/// not computed up front.
async fn indexed_rows(conn: &libsql::Connection, model: &ModelName) -> Result<usize> {
    let n: i64 = conn
        .query(&format!("SELECT COUNT(*) FROM {}", model.table()), ())
        .await?
        .next()
        .await?
        .map(|row| row.get(0))
        .transpose()?
        .unwrap_or(0);
    Ok(n.max(0) as usize)
}

/// Validate a vector against the model's declared dimension, then encode it.
///
/// The dimension comes from `F32_BLOB(n)` in the table's own column type, not
/// from the caller and not from a table this crate maintains. That matters: the
/// previous implementation called
/// `EmbeddingCodec::encode(query_vec, query_vec.len(), model)`, comparing the
/// length against itself, so the check was true by construction and
/// `DimMismatch` was unreachable through the search path.
async fn encode_for_model(
    conn: &libsql::Connection,
    model: &ModelName,
    vector: &[f32],
) -> Result<Vec<u8>> {
    let dim = declared_dimension(conn, model).await?;
    EmbeddingCodec::encode(vector, dim, model.as_str())
}

/// Compute Reciprocal Rank Fusion (RRF) score fusion algorithm: RRF(d) = \sum \frac{1}{k + r(d)} with k=60 (§5.9).
pub fn reciprocal_rank_fusion(
    vector_ranks: &[String],
    keyword_ranks: &[String],
    k: usize,
) -> Vec<(String, f64)> {
    let mut scores = HashMap::new();

    for (rank, id) in vector_ranks.iter().enumerate() {
        let score = 1.0 / ((k + rank + 1) as f64);
        *scores.entry(id.clone()).or_insert(0.0) += score;
    }

    for (rank, id) in keyword_ranks.iter().enumerate() {
        let score = 1.0 / ((k + rank + 1) as f64);
        *scores.entry(id.clone()).or_insert(0.0) += score;
    }

    let mut sorted: Vec<_> = scores.into_iter().collect();
    // Score descending, then id ascending. The tie-break is not cosmetic: ties
    // are the *common* case here, because two documents at the same pair of
    // ranks in the two arms score identically by construction, and symmetric
    // inputs (a document at rank 3 in one arm, another at rank 3 in the other)
    // tie exactly. Sorting on the score alone left those in `HashMap` iteration
    // order, so the same query could return the same set in a different order on
    // the next run — the procedural-versus-structural determinism trap D-047
    // names, arriving here as a search result that will not sit still.
    sorted.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    sorted
}
