use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryLifecycleStage {
    Candidate,
    Verified,
    Permanent,
    Archived,
}
