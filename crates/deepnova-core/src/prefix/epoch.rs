use crate::identity::hashing::PromptHash;
use chrono::{DateTime, Utc};

use super::state::EpochId;

#[derive(Debug, Clone)]
pub struct EpochCommit {
    pub id: EpochId,
    pub parent_id: Option<EpochId>,
    pub author: String, // e.g. "AgentPolicy", "SystemController", "MemoryPromoter"
    pub reason: String,
    pub impact: String,
    pub timestamp: DateTime<Utc>,
    pub diff_summary: String,
    pub new_prefix_hash: PromptHash,
}

impl EpochCommit {
    pub fn new(
        id: EpochId,
        parent_id: Option<EpochId>,
        author: String,
        reason: String,
        impact: String,
        diff_summary: String,
        new_prefix_hash: PromptHash,
    ) -> Self {
        Self {
            id,
            parent_id,
            author,
            reason,
            impact,
            timestamp: Utc::now(),
            diff_summary,
            new_prefix_hash,
        }
    }
}
