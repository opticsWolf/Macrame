use ulid::Ulid;
use crate::error::{DbError, Result};

/// Generate a new Crockford base32 26-character ULID string.
pub fn generate_id() -> String {
    Ulid::new().to_string()
}

/// Validate if a given string is a valid Crockford base32 ULID.
pub fn validate_id(id: &str) -> Result<()> {
    Ulid::from_string(id).map_err(|_| DbError::NotFound(id.to_string()))?;
    Ok(())
}
