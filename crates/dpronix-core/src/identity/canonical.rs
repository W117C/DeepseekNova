use serde::Serialize;
use anyhow::{Context, Result};

/// Serializes any Serde-compatible struct to a canonical JSON byte array
/// strictly following RFC 8785 (JSON Canonicalization Scheme).
/// 
/// This guarantees deterministic byte sequencing across all environments,
/// eliminating cache-busting from HashMap randomness, whitespace differences,
/// float formatting, and unicode encoding variations.
pub fn to_canonical_json<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>> {
    serde_jcs::to_vec(value)
        .context("Failed to strictly canonicalize struct per RFC 8785")
}
