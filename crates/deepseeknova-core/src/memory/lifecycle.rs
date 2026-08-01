//! # Memory Lifecycle
//!
//! Automatic promotion/demotion between lifecycle stages based on
//! recall frequency, age, and importance scoring.
//!
//! ## Stage Transitions
//!
//! ```text
//! Candidate ──(recalled ≥1)──▶ Verified ──(recalled ≥3 AND age >7d)──▶ Permanent
//!     │                            │
//!     └──(age >30d, never recalled)──▶ Archived ◀──(importance <0.2)──┘
//! ```

use chrono::Utc;
use serde::{Deserialize, Serialize};

/// Lifecycle stage of a memory entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

impl MemoryLifecycleStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Verified => "verified",
            Self::Permanent => "permanent",
            Self::Archived => "archived",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "verified" => Self::Verified,
            "permanent" => Self::Permanent,
            "archived" => Self::Archived,
            _ => Self::Candidate,
        }
    }

    /// Numeric priority for sorting (higher = more important).
    pub fn priority(&self) -> u8 {
        match self {
            Self::Permanent => 3,
            Self::Verified => 2,
            Self::Candidate => 1,
            Self::Archived => 0,
        }
    }
}

/// Metadata tracked per memory entry for lifecycle decisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleMeta {
    /// Current lifecycle stage.
    pub stage: MemoryLifecycleStage,
    /// Number of times this memory has been recalled.
    pub recall_count: u32,
    /// Timestamp of last recall (unix seconds).
    pub last_recalled_at: Option<i64>,
    /// Timestamp when the entry was created (unix seconds).
    pub created_at: i64,
    /// Importance score [0.0, 1.0].
    pub importance: f32,
}

impl LifecycleMeta {
    /// Create metadata for a newly stored memory.
    pub fn new(importance: f32) -> Self {
        Self {
            stage: MemoryLifecycleStage::Candidate,
            recall_count: 0,
            last_recalled_at: None,
            created_at: Utc::now().timestamp(),
            importance,
        }
    }

    /// Record a recall event. Returns true if the stage changed.
    pub fn record_recall(&mut self) -> bool {
        self.recall_count += 1;
        self.last_recalled_at = Some(Utc::now().timestamp());
        self.evaluate()
    }

    /// Re-evaluate the lifecycle stage based on current metadata.
    /// Returns true if the stage changed.
    pub fn evaluate(&mut self) -> bool {
        let old_stage = self.stage.clone();
        let now = Utc::now().timestamp();
        let age_days = (now - self.created_at) as f64 / 86_400.0;

        self.stage = if self.importance < 0.15 && self.recall_count == 0 && age_days > 30.0 {
            // Low importance, never recalled, old → archive
            MemoryLifecycleStage::Archived
        } else if self.recall_count >= 3 && age_days > 7.0 && self.importance >= 0.5 {
            // Frequently recalled, mature, high importance → permanent
            MemoryLifecycleStage::Permanent
        } else if self.recall_count >= 1 {
            // Recalled at least once → verified
            MemoryLifecycleStage::Verified
        } else if age_days > 30.0 && self.importance < 0.3 {
            // Aging out without validation
            MemoryLifecycleStage::Archived
        } else {
            MemoryLifecycleStage::Candidate
        };

        self.stage != old_stage
    }

    /// Apply time-based decay to the importance score.
    /// Called periodically (e.g., daily) to fade unused memories.
    /// Returns true if the entry was archived due to decay.
    pub fn apply_decay(&mut self, decay_rate: f32) -> bool {
        if self.stage == MemoryLifecycleStage::Permanent {
            return false; // Permanent memories don't decay
        }

        // Decay factor: recently recalled memories decay slower
        let recency_bonus = if let Some(last) = self.last_recalled_at {
            let days_since = (Utc::now().timestamp() - last) as f32 / 86_400.0;
            if days_since < 7.0 {
                0.5
            } else {
                1.0
            }
        } else {
            1.0
        };

        self.importance -= decay_rate * recency_bonus;
        self.importance = self.importance.max(0.0);

        // Archive if importance drops below threshold
        if self.importance < 0.1 && self.stage != MemoryLifecycleStage::Archived {
            self.stage = MemoryLifecycleStage::Archived;
            return true;
        }
        false
    }

    /// Boost importance when a memory is explicitly reinforced by the user.
    pub fn reinforce(&mut self, boost: f32) {
        self.importance = (self.importance + boost).min(1.0);
        // Reinforcement can pull out of archive
        if self.stage == MemoryLifecycleStage::Archived && self.importance >= 0.3 {
            self.stage = MemoryLifecycleStage::Candidate;
        }
        self.evaluate();
    }
}

/// Configuration for the lifecycle manager.
#[derive(Debug, Clone)]
pub struct LifecycleConfig {
    /// Daily importance decay rate for non-permanent memories.
    pub daily_decay_rate: f32,
    /// Minimum recall count for Verified promotion.
    pub min_recalls_for_verified: u32,
    /// Minimum recall count for Permanent promotion.
    pub min_recalls_for_permanent: u32,
    /// Minimum age in days for Permanent promotion.
    pub min_age_days_for_permanent: f64,
    /// Age in days after which unrecalled low-importance memories are archived.
    pub archive_after_days: f64,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            daily_decay_rate: 0.02,
            min_recalls_for_verified: 1,
            min_recalls_for_permanent: 3,
            min_age_days_for_permanent: 7.0,
            archive_after_days: 30.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_memory_starts_as_candidate() {
        let meta = LifecycleMeta::new(0.5);
        assert_eq!(meta.stage, MemoryLifecycleStage::Candidate);
        assert_eq!(meta.recall_count, 0);
    }

    #[test]
    fn test_recall_promotes_to_verified() {
        let mut meta = LifecycleMeta::new(0.5);
        let changed = meta.record_recall();
        assert!(changed);
        assert_eq!(meta.stage, MemoryLifecycleStage::Verified);
    }

    #[test]
    fn test_multiple_recalls_and_age_promotes_to_permanent() {
        let mut meta = LifecycleMeta::new(0.8);
        // Simulate old creation (10 days ago)
        meta.created_at = Utc::now().timestamp() - 10 * 86_400;
        meta.record_recall();
        meta.record_recall();
        meta.record_recall();
        assert_eq!(meta.stage, MemoryLifecycleStage::Permanent);
    }

    #[test]
    fn test_decay_reduces_importance() {
        let mut meta = LifecycleMeta::new(0.5);
        meta.apply_decay(0.1);
        assert!(meta.importance < 0.5);
    }

    #[test]
    fn test_permanent_does_not_decay() {
        let mut meta = LifecycleMeta::new(0.8);
        meta.created_at = Utc::now().timestamp() - 10 * 86_400;
        meta.record_recall();
        meta.record_recall();
        meta.record_recall();
        assert_eq!(meta.stage, MemoryLifecycleStage::Permanent);
        let before = meta.importance;
        meta.apply_decay(0.1);
        assert_eq!(meta.importance, before);
    }

    #[test]
    fn test_reinforce_pulls_from_archive() {
        let mut meta = LifecycleMeta::new(0.1);
        meta.created_at = Utc::now().timestamp() - 60 * 86_400;
        meta.evaluate();
        assert_eq!(meta.stage, MemoryLifecycleStage::Archived);
        meta.reinforce(0.5);
        assert_ne!(meta.stage, MemoryLifecycleStage::Archived);
    }

    #[test]
    fn test_stage_priority_ordering() {
        assert!(
            MemoryLifecycleStage::Permanent.priority() > MemoryLifecycleStage::Verified.priority()
        );
        assert!(
            MemoryLifecycleStage::Verified.priority() > MemoryLifecycleStage::Candidate.priority()
        );
        assert!(
            MemoryLifecycleStage::Candidate.priority() > MemoryLifecycleStage::Archived.priority()
        );
    }
}
