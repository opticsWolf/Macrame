use std::collections::HashMap;
use std::time::Duration;

use crate::error::{DbError, Result};
use crate::vector::registry::declared_dimension;
use crate::vector::{EmbeddingCodec, ModelName};

/// Search result container for vector similarity or hybrid search (§5.9).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
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

/// Store the vector of ONE overlay version row (0.22.0, M6-c, Amendment 2
/// §5.4 cost 3b): keyed by `(concept_id, branch_id, valid_from)`, the fold
/// channels' identity, so a re-derived vector of the same version replaces
/// its row instead of doubling it (the unique key the register created).
///
/// Doctrine VII is unchanged and it is the whole point: an embedding is
/// derived from content, *and a version is content*. The caller embeds the
/// version's own text, which is why the caller-driven shape of
/// `upsert_embedding` is kept rather than any trigger-time derivation -
/// the crate does not host models (Doctrine VII's reasoning about where
/// the vector comes from), so a knock-on vector at write time would be
/// the trunk's, which is exactly the defect the cost row names.
///
/// Like the trunk's twin this touches no ledger and no log; there are no
/// triggers on the version table.
///
/// # Prefer `Database::upsert_version_embeddings`
///
/// A bare connection is SS4.7 invariant 2's third hole, hidden from the
/// docs alongside `raw` (D-091) for the trunk twin's own reason.
///
/// [`DbError::DimMismatch`]: crate::error::DbError::DimMismatch
#[doc(hidden)]
pub async fn upsert_version_embedding(
    conn: &libsql::Connection,
    model: &ModelName,
    concept_id: &str,
    branch_id: &str,
    valid_from: &str,
    vector: &[f32],
) -> Result<()> {
    let blob = encode_for_model(conn, model, vector).await?;

    // `model.version_table()` is a bare identifier by construction; the
    // values bind.
    conn.execute(
        &format!(
            "INSERT INTO {table} (id, branch_id, valid_from, embedding) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(id, branch_id, valid_from) DO UPDATE SET \
                 embedding = excluded.embedding",
            table = model.version_table()
        ),
        libsql::params![concept_id, branch_id, valid_from, blob],
    )
    .await?;
    Ok(())
}

/// Store or replace one chunk of VERSION vectors for a model, in one
/// transaction (0.22.0, M6-c). The trunk twin's chunk discipline, carried
/// over: the dimension resolved once per chunk, the statement prepared
/// once, the rows reset-then-bound. What differs is the key the conflict
/// clause names - the trio the fold channels key by, not a `concept_id`
/// alone - which is what makes the same version's re-embedding a
/// replacement.
pub(crate) async fn upsert_version_embedding_chunk(
    conn: &libsql::Connection,
    model: &ModelName,
    rows: &[(String, String, String, Vec<f32>)],
) -> Result<usize> {
    if rows.is_empty() {
        return Ok(0);
    }

    let dim = declared_dimension(conn, model).await?;
    let sql = format!(
        "INSERT INTO {table} (id, branch_id, valid_from, embedding) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(id, branch_id, valid_from) DO UPDATE SET \
             embedding = excluded.embedding",
        table = model.version_table()
    );

    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;

    // Prepared once per chunk, for the same reason the dimension is resolved once
    // per chunk and the same reason the edge chunk hoists its insert (D-056).
    let stmt = tx.prepare(&sql).await?;

    let res: Result<()> = async {
        for (concept_id, branch_id, valid_from, vector) in rows {
            let blob = EmbeddingCodec::encode(vector, dim, model.as_str())?;
            stmt.reset();
            stmt.execute(libsql::params![
                concept_id.as_str(),
                branch_id.as_str(),
                valid_from.as_str(),
                blob
            ])
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

/// How deep a surface must read before it re-ranks, given a final `top_k`.
///
/// `max(5 × top_k, 50)` — [`crate::vector::HybridSearch::depth`]'s rule,
/// promoted to a function in 0.13.20 because decay needs it for the same
/// reason fusion does: **re-ranking a top-k is not the top-k of the
/// re-ranking.** A row the index ranked eleventh can outrank one it ranked
/// first once age is priced in, and it is invisible if the list was never read
/// past ten.
///
/// It is a bound and not a guarantee, and saying so is the honest form. Decay
/// only ever *demotes*, so a row outside the pool enters the answer only if
/// five times as many rows ahead of it were pushed below it — the same trade
/// `depth` has made since 0.5.5, priced as one larger `LIMIT` rather than a
/// second round trip.
pub(crate) fn rerank_depth(top_k: usize) -> usize {
    (top_k * 5).max(50)
}

/// `0.5 ^ (age / half_life)`: **1.0 at zero age, 0.5 at one half-life**, and
/// asymptotically zero after that.
///
/// `reference` is the instant age is measured *from* and `valid_from` is when
/// the concept became true, both canonical. A `valid_from` after `reference`
/// cannot arise where this is called — the same instant bounds the query —
/// and is clamped to zero age rather than trusted to underflow.
///
/// A zero half-life is the defined limit of the formula rather than an error:
/// anything with any age at all is fully decayed, and only what became true at
/// the instant itself survives. That is a strange thing to ask for and an
/// unambiguous one, which is the bar for not adding a refusal.
pub(crate) fn decay_factor(reference: &str, valid_from: &str, half_life: Duration) -> Result<f64> {
    let age = crate::util::timestamp::parse(reference)?
        .duration_since(crate::util::timestamp::parse(valid_from)?)
        .unwrap_or(Duration::ZERO);
    if half_life.is_zero() {
        return Ok(if age.is_zero() { 1.0 } else { 0.0 });
    }
    Ok(0.5f64.powf(age.as_secs_f64() / half_life.as_secs_f64()))
}

/// A cosine **distance**, decayed, still a distance (0.13.20, W9.5, D-193).
///
/// **This is the sign trap, and it is a conversion rather than a multiply.** A
/// decay factor in (0, 1] multiplied into a *similarity* penalises age
/// correctly; multiplied into a *distance* it makes stale rows look nearer.
/// `vector_distance_cos` returns `1 - cosθ` in [0, 2], so similarity is
/// `(2 - d) / 2` in [0, 1] — mapped to a non-negative range **before** the
/// multiply, because scaling a negative similarity toward zero would improve
/// it, which is the same trap wearing a second face.
///
/// The result is a distance again, so the surface's contract is unchanged:
/// smaller is better and the list still ascends. At `factor == 1.0` this is the
/// identity, which is the property `decay_is_the_identity_at_zero_age` pins.
pub(crate) fn decayed_distance(distance: f32, factor: f64) -> f32 {
    let similarity = ((2.0 - distance as f64) / 2.0).clamp(0.0, 1.0);
    (2.0 - 2.0 * similarity * factor) as f32
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
    half_life: Option<Duration>,
) -> Result<Vec<VectorSearchResult>> {
    search_vector_on(conn, query_vec, model, top_k, as_of_valid, half_life, None).await
}

/// [`search_vector`], restricted to the concepts one lineage holds (0.22.0,
/// M5, [D-310]).
///
/// `None` is [`search_vector`] exactly. `Some(b)` joins `concept_versions` for
/// `b` instead of the live table, so the escalation above re-asks the index
/// whenever the lineage's share of the nearest neighbours comes up short — the
/// same mechanism that already absorbs retirement and valid-time thinning. See
/// [`crate::vector::keyword_search_on`] for the shared semantics.
///
/// [D-310]: ../../docs/architecture/s13-decision-register.md#d-310
pub async fn search_vector_on(
    conn: &libsql::Connection,
    query_vec: &[f32],
    model: &ModelName,
    top_k: usize,
    as_of_valid: Option<&str>,
    half_life: Option<Duration>,
    branch: Option<&str>,
) -> Result<Vec<VectorSearchResult>> {
    if top_k == 0 {
        return Ok(Vec::new());
    }
    // Age is measured from the instant the search reads at, and there is no
    // other instant on this path to fall back to.
    let reference = match (half_life, as_of_valid) {
        (Some(_), None) => return Err(DbError::HalfLifeWithoutInstant),
        (Some(_), Some(t)) => Some(t),
        (None, _) => None,
    };
    let blob = encode_for_model(conn, model, query_vec).await?;

    // Decay reorders, so the pool that gets ranked has to be deeper than the
    // answer. With no half-life this is `top_k` and the statement is what
    // 0.13.19 issued, `valid_from` included.
    let want = match half_life {
        Some(_) => rerank_depth(top_k),
        None => top_k,
    };
    let age_column = if half_life.is_some() {
        ", c.valid_from"
    } else {
        ""
    };

    // `?2` is what the index is asked for and `?3` is what the ranking pool
    // needs; they are the same on the first pass and diverge on escalation.
    let scope = match branch {
        Some(b) => crate::graph::concept_lineage::scope_for(conn, Some(b)).await?,
        None => None,
    };
    // The overlay's switch, M6-c style (0.22.0, [Amendment 2]
    // (../docs/Macrame%20Update%20Plan%20v0.22.0%20-%20Amendment%202.md)
    // §5.4 costs 3 and 3b): the version pool and the resolution drop run
    // only on a forked ledger that has said something in the overlay. A
    // never-forked database, and a forked one with nothing versioned yet,
    // run the statement below byte for byte as it was issued in 0.13.19.
    let engaged: Option<Vec<crate::graph::lineage::Ancestor>> = match &scope {
        Some(ancestry) if crate::graph::concept_lineage::overlay_engaged(conn).await? => {
            Some(ancestry.clone())
        }
        _ => None,
    };

    if let Some(ancestry) = &engaged {
        return search_vector_on_overlay(
            conn,
            &blob,
            model,
            top_k,
            as_of_valid,
            half_life,
            want,
            ancestry,
        )
        .await;
    }

    let (with, source) = match &scope {
        None => (String::new(), "concepts"),
        Some(ancestry) => (
            format!(
                "WITH {} ",
                crate::graph::concept_lineage::versions_cte(
                    ancestry.len(),
                    4 + usize::from(as_of_valid.is_some()),
                    None,
                    "",
                    // The same probe the concept folds run (D-303; 0.22.0
                    // plan §3.2): a keyword search's lineage scope obeys the
                    // state rule on the same terms the concept read does —
                    // the two cannot agree on what is owed by disagreeing.
                    crate::temporal::replay::state_rule_owed(conn).await?,
                )
            ),
            "concept_versions",
        ),
    };
    let sql = format!(
        "{with}SELECT e.concept_id, vector_distance_cos(e.embedding, ?1){age_column}
           FROM vector_top_k('{index}', ?1, ?2) AS t
           JOIN {table} AS e ON e.rowid = t.id
           JOIN {source} AS c ON c.id = e.concept_id
          WHERE {visible}
          ORDER BY 2 ASC
          LIMIT ?3",
        index = model.index(),
        table = model.table(),
        visible = visible_concept(as_of_valid.map(|_| 4)),
    );

    let mut k_prime = want;
    let mut indexed: Option<usize> = None;

    loop {
        let mut params: Vec<libsql::Value> = vec![
            blob.clone().into(),
            (k_prime as i64).into(),
            (want as i64).into(),
        ];
        if let Some(t) = as_of_valid {
            params.push(t.into());
        }
        if let Some(ancestry) = &scope {
            params.extend(crate::graph::lineage::ancestry_params(ancestry));
        }
        let mut rows = conn.query(&sql, params).await?;

        let mut results = Vec::new();
        while let Some(row) = rows.next().await? {
            let hit = VectorSearchResult {
                concept_id: row.get(0)?,
                // The distance is computed by the engine over a non-null
                // F32_BLOB column, so a null here would mean the schema is not
                // what we think.
                score: row.get::<f64>(1)? as f32,
            };
            let valid_from: Option<String> = match reference {
                Some(_) => Some(row.get(2)?),
                None => None,
            };
            results.push((hit, valid_from));
        }

        if results.len() >= want {
            return rank_by_age(results, reference, half_life, top_k);
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
            return rank_by_age(results, reference, half_life, top_k);
        }
        k_prime = k_prime.saturating_mul(2).min(n);
    }
}

/// `search_vector_on`'s engaged half (0.22.0, M6-c, [Amendment 2]
/// (../docs/Macrame%20Update%20Plan%20v0.22.0%20-%20Amendment%202.md) §5.4
/// costs 3 and 3b).
///
/// Two pools answer a branch query now: the trunk index's `vector_top_k`
/// (the mint vectors the file has always held), and the version pool's own
/// `vector_top_k` over `embeddings_<m>_versions` — a version's text is
/// content under Doctrine VII, so a version has its own vector once the
/// caller embedded it, and scoring it with the trunk's row is the
/// wrong-text defect cost 3b names. The union is then judged by resolution
/// (the walk, M6-b's): a trunk-pool hit whose id resolves to a version is
/// the wrong-text hit; a version-pool hit whose id resolves to another
/// version is likewise; ids resolved absent are dropped from both. At most
/// one hit per id survives — the two arm-keeping predicates are exclusive
/// by the law that judges them.
///
/// The escalation is the trunk one's, deepened by version rows: `k'`
/// doubles until every visible neighbour the two indexes hold is in the
/// pool, which is where `k'` at the larger of the two row counts says
/// closer to exhaustion than the trunk count alone did. Termination is by
/// exhaustion, as it has been.
///
/// The decay pool's valid_from is the **resolved** row's window start in
/// both arms — a version's own start on the version pool, the mint row's
/// on the trunk pool — which is the same number each row carried before
/// the union existed.
// The overlay arm's signature is deliberately the trunk arm's shape plus the
// ancestry: call sites pass every arg positionally, and a struct to carry
// them would exist for two call sites and put a name between the statement
// and the params it already spells out.
#[allow(clippy::too_many_arguments)]
async fn search_vector_on_overlay(
    conn: &libsql::Connection,
    blob: &[u8],
    model: &ModelName,
    top_k: usize,
    as_of_valid: Option<&str>,
    half_life: Option<Duration>,
    want: usize,
    ancestry: &[crate::graph::lineage::Ancestor],
) -> Result<Vec<VectorSearchResult>> {
    let reference = match half_life {
        Some(_) => as_of_valid,
        None => None,
    };
    // The trunk arm: the same statement the unengaged path issues, WITH
    // list and all (D-303's state-rule probe included, for the same §3.2
    // shape reason its caller cites).
    let with = format!(
        "WITH {} ",
        crate::graph::concept_lineage::versions_cte(
            ancestry.len(),
            4 + usize::from(as_of_valid.is_some()),
            None,
            "",
            crate::temporal::replay::state_rule_owed(conn).await?,
        )
    );
    let trunk_sql = format!(
        "{with}SELECT e.concept_id, vector_distance_cos(e.embedding, ?1), c.valid_from
           FROM vector_top_k('{index}', ?1, ?2) AS t
           JOIN {table} AS e ON e.rowid = t.id
           JOIN concept_versions AS c ON c.id = e.concept_id
          WHERE {visible}
          ORDER BY 2 ASC
          LIMIT ?3",
        index = model.index(),
        table = model.table(),
        visible = visible_concept(as_of_valid.map(|_| 4)),
    );
    // The version arm: the index the register built beside the trunk's, so
    // the same `vector_top_k` shape answers in the same budget class. Nothing
    // else joins it: the resolution drop judges the pairs, not a view.
    let version_sql = format!(
        "SELECT ve.id, vector_distance_cos(ve.embedding, ?1), ve.branch_id, ve.valid_from
           FROM vector_top_k('{index}', ?1, ?2) AS t
           JOIN {table} AS ve ON ve.rowid = t.id
          ORDER BY 2 ASC
          LIMIT ?3",
        index = model.version_index(),
        table = model.version_table(),
    );

    let mut k_prime = want;
    let mut indexed: Option<usize> = None;
    let mut version_rows: Option<usize> = None;

    loop {
        // The trunk pool.
        let mut params: Vec<libsql::Value> = vec![
            blob.to_vec().into(),
            (k_prime as i64).into(),
            (want as i64).into(),
        ];
        if let Some(instant) = as_of_valid {
            params.push(instant.into());
        }
        params.extend(crate::graph::lineage::ancestry_params(ancestry));
        let mut trunk_hits: Vec<VectorSearchResult> = Vec::new();
        {
            let mut rows = conn.query(&trunk_sql, params).await?;
            while let Some(row) = rows.next().await? {
                trunk_hits.push(VectorSearchResult {
                    concept_id: row.get(0)?,
                    score: row.get::<f64>(1)? as f32,
                });
            }
        }

        // The version pool. A version without a row here is not a vector
        // result, the same honesty `search_vector` gives a concept without
        // one (Doctrine VII: absence means the derived artifact was not
        // written, not that the belief is unreachable).
        let mut version_hits: Vec<(String, String, VectorSearchResult)> = Vec::new();
        {
            let params: Vec<libsql::Value> = vec![
                blob.to_vec().into(),
                (k_prime as i64).into(),
                (want as i64).into(),
            ];
            let mut rows = conn.query(&version_sql, params).await?;
            while let Some(row) = rows.next().await? {
                version_hits.push((
                    row.get::<String>(2)?,
                    row.get::<String>(3)?,
                    VectorSearchResult {
                        concept_id: row.get(0)?,
                        score: row.get::<f64>(1)? as f32,
                    },
                ));
            }
        }

        // The resolution drop: one walk over the union's id set; each hit is
        // kept only when the resolution is the text/vector the hit carries
        // (§5.4's cost-3 sentence: "drops every hit whose id resolves to a
        // different version or to absent").
        let mut asked: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for hit in trunk_hits.iter() {
            asked.insert(hit.concept_id.clone());
        }
        for (id, _, _) in version_hits.iter() {
            asked.insert(id.clone());
        }
        let ids: Vec<String> = asked.into_iter().collect();
        let (answers, _absent) =
            crate::graph::concept_lineage::rows_on(conn, &ids, ancestry, None, as_of_valid).await?;

        let mut pool: Vec<(VectorSearchResult, Option<String>)> = Vec::new();
        for hit in trunk_hits {
            // A trunk-pool hit is the mint's vector: it survives only where
            // the walk answers with the mint's row.
            if let Some(ans) = answers.get(hit.concept_id.as_str()) {
                if !ans.versioned {
                    pool.push((hit, ans.valid_from.clone()));
                }
            }
        }
        for (branch_id, valid_from, hit) in version_hits {
            // A version-pool hit is that pair's vector: it survives only
            // where the walk answers with that pair.
            if let Some(ans) = answers.get(hit.concept_id.as_str()) {
                if ans.versioned
                    && ans.branch_id == branch_id
                    && ans.valid_from.as_deref() == Some(valid_from.as_str())
                {
                    let base = ans.valid_from.clone();
                    pool.push((hit, base));
                }
            }
        }

        if pool.len() >= want {
            return finalize_pool(pool, reference, half_life, top_k);
        }

        // The pools' caps, read at most once each and only on the escalating
        // path: the trunk arm's index count, and the version table's row
        // count, as `vector_top_k` already asks by `k'`.
        let n1 = match &indexed {
            Some(n) => *n,
            None => {
                let n = indexed_rows(conn, model).await?;
                indexed = Some(n);
                n
            }
        };
        let n2 = match &version_rows {
            Some(n) => *n,
            None => {
                let n = version_row_count(conn, model).await?;
                version_rows = Some(n);
                n
            }
        };
        // Each arm has been asked for at least its own count, so what the two
        // pools hold is every visible neighbour there is.
        if k_prime >= n1.max(n2) {
            return finalize_pool(pool, reference, half_life, top_k);
        }
        k_prime = k_prime.saturating_mul(2).min(n1.max(n2));
    }
}

/// Order a merged pool, cut it to the answer, decay where the caller asked.
///
/// The merged pool arrives with both arms' rows at a `LIMIT` this function no
/// longer trusts, so the identity case orders by (score, id) itself — the
/// ordering the single-arm SQL did `ORDER BY 2 ASC` — and truncates. The
/// decay case delegates to `rank_by_age`, which already does both.
fn finalize_pool(
    pool: Vec<(VectorSearchResult, Option<String>)>,
    reference: Option<&str>,
    half_life: Option<Duration>,
    top_k: usize,
) -> Result<Vec<VectorSearchResult>> {
    if reference.is_none() || half_life.is_none() {
        let mut out: Vec<VectorSearchResult> = pool.into_iter().map(|(h, _)| h).collect();
        out.sort_by(|a, b| {
            a.score
                .partial_cmp(&b.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.concept_id.cmp(&b.concept_id))
        });
        out.truncate(top_k);
        return Ok(out);
    }
    rank_by_age(pool, reference, half_life, top_k)
}

/// The version pool's row count, the escalation cap's second half.
///
/// One `COUNT(*)` over the version table, read at most once per search and
/// only on the escalating path — the same economy `indexed_rows` keeps, and
/// the reason the count is here rather than a registry: D-005's non-registry
/// is a fact about this crate, and the table plus its F32_BLOB declaration
/// is the registry the schema itself maintains.
async fn version_row_count(conn: &libsql::Connection, model: &ModelName) -> Result<usize> {
    let sql = format!("SELECT COUNT(*) FROM {}", model.version_table());
    let mut rows = conn.query(&sql, ()).await?;
    let n: i64 = rows
        .next()
        .await?
        .map(|r| r.get(0))
        .transpose()?
        .unwrap_or(0);
    Ok(n.max(0) as usize)
}
/// Apply decay to a retrieved pool, reorder, and cut it to `top_k`.
///
/// With no half-life this is the identity but for the truncation, and the
/// truncation is already the `LIMIT`: the rows arrive ordered from the engine
/// and are handed back untouched, which is what keeps 0.13.19's answer exactly
/// 0.13.19's answer.
///
/// With one, the sort breaks ties on the id. Two rows at an identical decayed
/// distance must not swap between runs, or the same query answers differently
/// on two machines — `run_pre_filter` merges its chunks under the same rule.
fn rank_by_age(
    results: Vec<(VectorSearchResult, Option<String>)>,
    reference: Option<&str>,
    half_life: Option<Duration>,
    top_k: usize,
) -> Result<Vec<VectorSearchResult>> {
    let (Some(reference), Some(half_life)) = (reference, half_life) else {
        return Ok(results.into_iter().map(|(hit, _)| hit).collect());
    };

    let mut out = Vec::with_capacity(results.len());
    for (mut hit, valid_from) in results {
        // Selected on this path and only on this path, so its absence is a
        // programming error rather than a row that lacks a validity.
        let valid_from = valid_from.unwrap_or_default();
        let factor = decay_factor(reference, &valid_from, half_life)?;
        hit.score = decayed_distance(hit.score, factor);
        out.push(hit);
    }
    out.sort_by(|a, b| {
        a.score
            .partial_cmp(&b.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.concept_id.cmp(&b.concept_id))
    });
    out.truncate(top_k);
    Ok(out)
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

#[cfg(test)]
mod decay_tests {
    use super::*;

    const HOUR: Duration = Duration::from_secs(3600);
    const T0: &str = "2026-01-01T00:00:00.000000Z";
    const T1: &str = "2026-01-01T01:00:00.000000Z";
    const T2: &str = "2026-01-01T02:00:00.000000Z";

    /// The definition, at the two points where it is a definition rather than
    /// an interpolation.
    #[test]
    fn a_half_life_halves_at_a_half_life() {
        assert_eq!(decay_factor(T0, T0, HOUR).unwrap(), 1.0);
        assert_eq!(decay_factor(T1, T0, HOUR).unwrap(), 0.5);
        assert_eq!(decay_factor(T2, T0, HOUR).unwrap(), 0.25);
    }

    /// A concept that becomes true after the instant the search reads at cannot
    /// reach this on any real path — the same instant bounds the query — and
    /// clamps rather than underflowing if one ever does.
    #[test]
    fn a_future_validity_is_zero_age_rather_than_negative() {
        assert_eq!(decay_factor(T0, T1, HOUR).unwrap(), 1.0);
    }

    /// The limit of the formula, defined rather than refused: everything with
    /// any age at all is gone, and only what began at the instant survives.
    #[test]
    fn a_zero_half_life_is_the_limit_and_not_a_nan() {
        assert_eq!(decay_factor(T0, T0, Duration::ZERO).unwrap(), 1.0);
        assert_eq!(decay_factor(T1, T0, Duration::ZERO).unwrap(), 0.0);
    }

    /// **The sign, stated as arithmetic.** An undecayed hit is unchanged, and a
    /// decayed one is *further away* — never nearer, which is what multiplying
    /// the distance would have produced.
    #[test]
    fn decay_moves_a_hit_away_and_never_toward() {
        let near = 0.2_f32;
        assert_eq!(decayed_distance(near, 1.0), near);
        assert!(decayed_distance(near, 0.5) > near);
        assert!(decayed_distance(near, 0.01) > decayed_distance(near, 0.5));
        // Bounded by the far end of the cosine range rather than running away.
        assert!(decayed_distance(near, 0.0) <= 2.0);
    }

    /// Order within one age is the raw order: decay reprices, it does not
    /// reshuffle what it has not aged differently.
    #[test]
    fn one_factor_preserves_the_distance_order() {
        assert!(decayed_distance(0.1, 0.7) < decayed_distance(0.9, 0.7));
    }
}
