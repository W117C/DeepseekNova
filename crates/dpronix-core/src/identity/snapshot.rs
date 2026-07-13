use crate::identity::hashing::PromptHash;

pub enum SnapshotMode {
    Strict,
    Evolution,
}

pub struct GoldenSnapshot {
    pub expected_hash: PromptHash,
    pub mode: SnapshotMode,
}

impl GoldenSnapshot {
    pub fn verify(&self, actual_hash: &PromptHash) -> Result<(), String> {
        if &self.expected_hash == actual_hash {
            Ok(())
        } else {
            match self.mode {
                SnapshotMode::Strict => {
                    Err(format!("CRITICAL: Strict Snapshot Mismatch! Expected: {}, Actual: {}", self.expected_hash, actual_hash))
                }
                SnapshotMode::Evolution => {
                    // Log a warning and allow migration report generation, but don't hard fail.
                    Err(format!("WARNING: Evolution Snapshot Mismatch. Expected: {}, Actual: {}", self.expected_hash, actual_hash))
                }
            }
        }
    }
}
