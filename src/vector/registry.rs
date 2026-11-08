use crate::error::{DbError, Result};
use crate::schema::ddl;
use crate::vector::ModelName;

/// Create a model's embedding table and its DiskANN index, if absent (§4.1).
///
/// Both objects, or neither. The index is not an optimisation that can be added
/// later at leisure: until it exists the engine accepts vectors of any length
/// into the column (see [`ddl::create_embeddings_index`]), so a table without
/// its index is a table with no dimension enforcement at all. Creating them in
/// one transaction is what makes "registered" mean "enforcing".
///
/// Idempotent, so an application may call it on every start.
///
/// # Prefer [`crate::Database::register_model`]
///
/// This takes a **bare connection** and is therefore §4.7 invariant 2's third
/// hole: a write that does not cross the actor's channel. Hidden from the docs
/// alongside [`crate::Database::raw`] (D-091) so the documented path is the one
/// that preserves the single-writer property; still public, because
/// [D-068](../../docs/architecture/s13-decision-register.md#d-068)'s argument
/// applies unchanged — the file is reachable by any SQLite client, so removing
/// the supported way to do this would buy the appearance of a guarantee.
#[doc(hidden)]
pub async fn register_model(
    conn: &libsql::Connection,
    model: &ModelName,
    dim: usize,
) -> Result<()> {
    if dim == 0 {
        return Err(DbError::DimMismatch {
            got: 0,
            expected: 1,
            model: model.to_string(),
        });
    }

    // If it already exists at a different dimension, say so rather than
    // no-opping through `IF NOT EXISTS` and leaving the caller believing the
    // dimension they asked for is the one in force.
    if let Some(existing) = read_declared_dimension(conn, model).await? {
        if existing != dim {
            return Err(DbError::DimMismatch {
                got: dim,
                expected: existing,
                model: model.to_string(),
            });
        }
        return Ok(());
    }

    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let res: Result<()> = async {
        tx.execute(&ddl::create_embeddings_table(model, dim), ())
            .await?;
        tx.execute(&ddl::create_embeddings_index(model), ()).await?;
        // The version pool arrives with the pair it belongs to (0.22.0,
        // M6-c, Amendment 2 SS5.4 cost 3b): a model registered before the
        // v28 rung has no version table yet, and a search that could see
        // one but must reaching `INSERT`ing into a table that was never
        // created would run a branch vector search into `no such table`.
        tx.execute(&ddl::create_version_embeddings_table(model, dim), ())
            .await?;
        tx.execute(&ddl::create_version_embeddings_index(model), ())
            .await?;
        tx.execute(&ddl::create_version_embeddings_key(model), ())
            .await?;
        Ok(())
    }
    .await;

    match res {
        Ok(()) => {
            tx.commit().await?;
            Ok(())
        }
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e)
        }
    }
}

/// Create the version trio for every model whose table already exists,
/// in one pass (0.22.0, M6-c; the v28 rung's discovery step).
///
/// The crate keeps no model registry of its own (the same reasoning the
/// doc above gives), so the walk discovers registered models the way
/// `registered_models` does - by table name, `embeddings_*` not being a
/// version table itself - and validates each suffix through
/// `ModelName::new` before splicing a thing into DDL: a stray table with
/// an unbuildable name is skipped rather than turned into SQL. The
/// dimension comes from the trunk table's own `F32_BLOB` declaration
/// (`PRAGMA table_info`), so the trio cannot disagree with the pair it
/// extends. Idempotent, like `register_model` itself: a rung that
/// replays or a caller who runs it twice pays probes, not objects.
pub async fn backfill_version_tables(conn: &libsql::Connection) -> Result<()> {
    for model in registered_models(conn).await? {
        if read_declared_dimension(conn, &model).await?.is_none() {
            continue;
        }
        let dim = read_declared_dimension(conn, &model).await?.unwrap();
        conn.execute(&ddl::create_version_embeddings_table(&model, dim), ())
            .await?;
        conn.execute(&ddl::create_version_embeddings_index(&model), ())
            .await?;
        conn.execute(&ddl::create_version_embeddings_key(&model), ())
            .await?;
    }
    Ok(())
}

/// The dimension a registered model's table declares, read from the schema.
///
/// The crate keeps no registry of its own. `F32_BLOB(768)` in the column type
/// *is* the declaration, `PRAGMA table_info` reports it verbatim, and reading it
/// back means the Rust-side dimension check and the storage-layer one cannot
/// disagree — there is only one number. Keeping a `HashMap<model, dim>` beside
/// the database would be the same mistake D-035 fixed in `archive()`: a second
/// hand-maintained description of a set the schema already defines.
pub async fn declared_dimension(conn: &libsql::Connection, model: &ModelName) -> Result<usize> {
    read_declared_dimension(conn, model)
        .await?
        .ok_or_else(|| DbError::ModelNotRegistered {
            model: model.to_string(),
            table: model.table(),
        })
}

/// Drop a model's DiskANN index, tolerating its absence (bulk-embedding
/// setup, D-276).
///
/// Half of [`crate::Database::bulk_embeddings`]'s recipe, reachable through the
/// actor and nowhere else: dropping the index is what makes a bulk load pay
/// for table writes only (~5 µs/row, flat at every dimension measured) instead
/// of DiskANN maintenance (~10 ms/row at dim 256 and growing with the index).
/// `IF EXISTS` because the recipe is also the recovery path — a load that
/// failed before the drop is still a load whose finish rebuilds.
///
/// **The window is the caller's to close.** Between this and
/// [`rebuild_embedding_index`] the model's vectors are not searchable and not
/// dimension-checked at the storage layer; only the crate's own encode-time
/// check (`EmbeddingCodec::encode`) stands, and it stands for everything that
/// reaches storage through the crate. That trade is D-276's measured decision,
/// not a silent default — see the register entry for the numbers and the
/// Rejected line for keeping the index during loads.
#[doc(hidden)]
pub async fn drop_embedding_index(conn: &libsql::Connection, model: &ModelName) -> Result<()> {
    conn.execute(&format!("DROP INDEX IF EXISTS {}", model.index()), ())
        .await
        .map(|_| ())
        .map_err(Into::into)
}

/// Rebuild a model's DiskANN index in one pass (bulk-embedding finish, D-276).
///
/// One `CREATE INDEX` statement over the table's contents — a **one-pass
/// DiskANN build**, measured at **2.61 s** for 2,000 vectors at dim 64,
/// **19.7 s** at dim 256 and **39.0 s** at dim 512, versus 3.78 / 31.0 / 56.0 s
/// for the same rows inserted through the indexed path (this box, release
/// build, medians of three). It is one statement and has no smaller unit, so
/// the hold is what it is: the actor is busy for the build's whole duration,
/// and a search arriving meanwhile reports the missing index rather than
/// waiting. The same trade [`Database::analyze`] documents for its own
/// indivisible statement (D-166), priced in bulk-embedding currency.
///
/// `IF NOT EXISTS` so the rebuild is idempotent — a caller who bulk-loads twice
/// without an intervening drop, or a failure path that rebuilds twice, lands
/// the same index either way.
#[doc(hidden)]
pub async fn rebuild_embedding_index(conn: &libsql::Connection, model: &ModelName) -> Result<()> {
    conn.execute(&ddl::create_embeddings_index(model), ())
        .await
        .map(|_| ())
        .map_err(Into::into)
}

/// `Ok(None)` when the model has no table; `Ok(Some(dim))` when it has one.
async fn read_declared_dimension(
    conn: &libsql::Connection,
    model: &ModelName,
) -> Result<Option<usize>> {
    // `model` is a ModelName, so this splice is a bare identifier by construction.
    let mut rows = conn
        .query(&format!("PRAGMA table_info({})", model.table()), ())
        .await?;

    while let Some(row) = rows.next().await? {
        let name: String = row.get(1)?;
        if name == "embedding" {
            let declared: String = row.get(2)?;
            return parse_f32_blob_dim(&declared).map(Some).ok_or_else(|| {
                DbError::ModelNotRegistered {
                    model: model.to_string(),
                    table: format!(
                        "{} (column `embedding` is declared {declared:?}, not F32_BLOB(n))",
                        model.table()
                    ),
                }
            });
        }
    }
    Ok(None)
}

/// Pull `n` out of a declared type of the form `F32_BLOB(n)`.
fn parse_f32_blob_dim(declared: &str) -> Option<usize> {
    let t = declared.trim();
    if !t.get(..9)?.eq_ignore_ascii_case("F32_BLOB(") || !t.ends_with(')') {
        return None;
    }
    t[9..t.len() - 1].trim().parse::<usize>().ok()
}

/// Every model registered in this database, in name order.
///
/// Derived from `sqlite_master` for the same reason as the dimension: the set of
/// registered models is a fact about the schema, and asking the schema cannot
/// drift from it.
///
/// # `GLOB` rather than `LIKE`, and one filter rather than two (0.15.19, C-15)
///
/// `LIKE` treats `_` as *any single character*, and both arms of this filter
/// contained one. The prefix arm was harmless — `embeddingsX_foo` would have
/// matched, and the `strip_prefix` below rejects it anyway — but the exclusion
/// arm was not: `NOT LIKE '%_shadow'` reads as *"anything ending in shadow,
/// with any character before it"*, so a perfectly ordinary model named
/// `ashadow` or `bigshadow` was **silently missing from this list**, and from
/// everything that asks it what is registered. A model that exists, works, and
/// does not appear in its own registry is the shape of defect this function was
/// written to make impossible.
///
/// `GLOB`'s `_` is a literal, so the prefix says what it looks like. It is also
/// case-sensitive, which costs nothing here and is the stricter reading:
/// [`ModelName`] admits lowercase ASCII, digits and `_` only, so every table
/// this crate creates is `embeddings_<model>` in exactly that case.
///
/// The shadow exclusion is gone rather than escaped. libSQL's internals are
/// `libsql_vector_meta_shadow` and `<index>_shadow`, and this crate's indexes
/// are named `idx_embeddings_<model>_vec` — so none of them start with
/// `embeddings_` and the prefix already excludes every one. Escaping the
/// underscore would have kept a filter that can only ever fire on a real model
/// whose name happens to end in `_shadow`, which is the same defect one
/// character narrower.
///
/// The **version-table exclusion is a naming rule, not a filter refinement**
/// (0.22.0, M6-c): `embeddings_<m>_versions` is the version pool of model
/// `<m>`, and `represents a registered model` is true of the trunk table
/// only. `ModelName::new` refuses a name ending in `_versions` for the same
/// reason - a registered model and a version pool never share a walk.
pub async fn registered_models(conn: &libsql::Connection) -> Result<Vec<ModelName>> {
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master
              WHERE type = 'table'
                AND name GLOB 'embeddings_*'
                AND name NOT GLOB 'embeddings_*_versions'
              ORDER BY name",
            (),
        )
        .await?;

    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        let table: String = row.get(0)?;
        if let Some(suffix) = table.strip_prefix("embeddings_") {
            // A table matching the pattern but not the naming rule was not
            // created by `register_model`; skipping it is more honest than
            // reporting a name this crate would refuse to accept back.
            if let Ok(model) = ModelName::new(suffix) {
                out.push(model);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::parse_f32_blob_dim;

    #[test]
    fn reads_the_dimension_out_of_a_declared_type() {
        assert_eq!(parse_f32_blob_dim("F32_BLOB(768)"), Some(768));
        assert_eq!(parse_f32_blob_dim("f32_blob(1536)"), Some(1536));
        assert_eq!(parse_f32_blob_dim("  F32_BLOB( 4 )  "), Some(4));
    }

    /// Anything that is not a dimensioned vector column must read as absent
    /// rather than as some default, or a plain BLOB column would silently
    /// acquire whatever dimension the caller happened to pass.
    #[test]
    fn refuses_to_invent_a_dimension() {
        for bad in [
            "BLOB",
            "TEXT",
            "F32_BLOB",
            "F32_BLOB()",
            "F32_BLOB(x)",
            "F32_BLOB(4",
            "",
        ] {
            assert_eq!(parse_f32_blob_dim(bad), None, "{bad:?} yielded a dimension");
        }
    }
}
