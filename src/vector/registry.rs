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
            return parse_f32_blob_dim(&declared)
                .map(Some)
                .ok_or_else(|| DbError::ModelNotRegistered {
                    model: model.to_string(),
                    table: format!(
                        "{} (column `embedding` is declared {declared:?}, not F32_BLOB(n))",
                        model.table()
                    ),
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
/// drift from it. libSQL's own `libsql_vector_meta_shadow` and `*_shadow` tables
/// are filtered out — they are engine internals that happen to live in `main`.
pub async fn registered_models(conn: &libsql::Connection) -> Result<Vec<ModelName>> {
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master
              WHERE type = 'table'
                AND name LIKE 'embeddings_%'
                AND name NOT LIKE '%_shadow'
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
        for bad in ["BLOB", "TEXT", "F32_BLOB", "F32_BLOB()", "F32_BLOB(x)", "F32_BLOB(4", ""] {
            assert_eq!(parse_f32_blob_dim(bad), None, "{bad:?} yielded a dimension");
        }
    }
}
