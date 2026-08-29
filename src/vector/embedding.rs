use crate::error::{DbError, Result};

/// Codec for converting between `Vec<f32>` and libSQL F32_BLOB byte vectors with dimension validation (§4.1).
pub struct EmbeddingCodec;

impl EmbeddingCodec {
    /// Encode a float slice into a little-endian F32_BLOB byte vector, validating dimension against model declaration.
    pub fn encode(vec: &[f32], expected_dim: usize, model_name: &str) -> Result<Vec<u8>> {
        if vec.len() != expected_dim {
            return Err(DbError::DimMismatch {
                got: vec.len(),
                expected: expected_dim,
                model: model_name.to_string(),
            });
        }
        let mut bytes = Vec::with_capacity(vec.len() * 4);
        for &val in vec {
            bytes.extend_from_slice(&val.to_le_bytes());
        }
        Ok(bytes)
    }

    /// Decode an F32_BLOB byte vector back into `Vec<f32>`.
    pub fn decode(bytes: &[u8]) -> Result<Vec<f32>> {
        if !bytes.len().is_multiple_of(4) {
            return Err(DbError::ReplayCorrupt {
                seq: 0,
                reason: "Invalid F32_BLOB byte length".to_string(),
            });
        }
        let mut vec = Vec::with_capacity(bytes.len() / 4);
        for chunk in bytes.as_chunks::<4>().0 {
            let val = f32::from_le_bytes(*chunk);
            vec.push(val);
        }
        Ok(vec)
    }
}
