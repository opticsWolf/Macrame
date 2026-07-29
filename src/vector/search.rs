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
/// [`DimMismatch`]: crate::error::DbError::DimMismatch
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

    let res: Result<()> = async {
        for (concept_id, vector) in rows {
            let blob = EmbeddingCodec::encode(vector, dim, model.as_str())?;
            tx.execute(&sql, libsql::params![concept_id.as_str(), blob])
                .await?;
        }
        Ok(())
    }
    .await;

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

/// Top-k nearest neighbours for `query_vec` under `model` (§5.9).
///
/// Goes through `vector_top_k`, which consults the DiskANN index, rather than
/// scanning the table and sorting: the index is what §9's "top-10 over 100K
/// concepts in ≤20 ms" budget assumes, and an `ORDER BY vector_distance_cos(…)`
/// over the whole table is linear in the corpus no matter how small `k` is.
/// `vector_top_k` yields base-table rowids, so the distance is recomputed on the
/// k rows it selects — k distance evaluations, not one per concept.
pub async fn search_vector(
    conn: &libsql::Connection,
    query_vec: &[f32],
    model: &ModelName,
    top_k: usize,
) -> Result<Vec<VectorSearchResult>> {
    if top_k == 0 {
        return Ok(Vec::new());
    }
    let blob = encode_for_model(conn, model, query_vec).await?;

    let sql = format!(
        "SELECT e.concept_id, vector_distance_cos(e.embedding, ?1)
           FROM vector_top_k('{index}', ?1, ?2) AS t
           JOIN {table} AS e ON e.rowid = t.id
          ORDER BY 2 ASC",
        index = model.index(),
        table = model.table(),
    );

    let mut rows = conn
        .query(&sql, libsql::params![blob, top_k as i64])
        .await?;

    let mut results = Vec::new();
    while let Some(row) = rows.next().await? {
        results.push(VectorSearchResult {
            concept_id: row.get(0)?,
            // The distance is computed by the engine over a non-null F32_BLOB
            // column, so a null here would mean the schema is not what we think.
            score: row.get::<f64>(1)? as f32,
        });
    }
    Ok(results)
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
    sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    sorted
}
