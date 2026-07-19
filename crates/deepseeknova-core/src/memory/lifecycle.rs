//! # Memory Lifecycle
//!
//! **Experimental** — defines memory promotion stages but does not yet
//! implement state transitions or enforcement logic.
//!
//! Planned: automatic promotion/demotion between stages based on
//! recall frequency, age, and importance scoring.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryLifecycleStage {
    /// Newly stored, not yet validated.
    Candidate,
    /// Confirmed as useful (recalled at least once).
    Verified,
    /// Promoted to long-term retention.
    Permanent,
    /// Deprecated but retained for audit.
    Archived,
}
