use sha2::{Digest, Sha256};
use std::fmt;

/// Represents a cryptographic hash of a Prompt Identity component.
#[derive(Clone, PartialEq, Eq, Hash, Default)]
pub struct PromptHash(pub [u8; 32]);

impl fmt::Debug for PromptHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_hex())
    }
}

impl fmt::Display for PromptHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_hex())
    }
}

impl PromptHash {
    pub fn as_hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn short_hex(&self) -> String {
        let full = self.as_hex();
        full.chars().take(8).collect()
    }
}

/// Abstract hashing trait to allow swapping the hashing algorithm (e.g., to BLAKE3).
pub trait PromptHasher {
    fn hash(bytes: &[u8]) -> PromptHash;
}

/// The default production hasher (SHA-256).
pub struct DefaultHasher;

impl PromptHasher for DefaultHasher {
    fn hash(bytes: &[u8]) -> PromptHash {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let result = hasher.finalize();
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&result);
        PromptHash(hash)
    }
}
