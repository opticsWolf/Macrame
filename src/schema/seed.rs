use crate::error::Result;

/// Optional bootstrap seeding utilities.
pub async fn seed_bootstrap(_conn: &libsql::Connection) -> Result<()> {
    Ok(())
}
