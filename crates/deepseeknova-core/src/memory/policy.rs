use super::evidence::MemoryEvidence;
use super::lifecycle::MemoryLifecycleStage;
use chrono::Utc;

/// 记忆晋升策略：基于证据（频次/置信度/年龄）评估阶段迁移。
pub struct MemoryPromotionPolicy {
    /// 晋升 Permanent 所需的最小观察频次。
    pub min_frequency: u32,
    /// 晋升 Permanent 所需的最小置信度。
    pub min_confidence: f64,
    /// 晋升 Permanent 所需的最小首次观察年龄（天）。
    pub min_age_days: i64,
}

impl Default for MemoryPromotionPolicy {
    fn default() -> Self {
        Self {
            min_frequency: 5,
            min_confidence: 0.8,
            min_age_days: 7,
        }
    }
}

impl MemoryPromotionPolicy {
    /// 依据当前阶段与证据评估目标阶段：Candidate 在频次/置信度达标后晋升 Verified；
    /// Verified 在频次/置信度/年龄三者均达标后晋升 Permanent；Permanent 在
    /// 90 天未观察后降为 Archived；其余维持原阶段。
    pub fn evaluate(
        &self,
        current_stage: &MemoryLifecycleStage,
        evidence: &MemoryEvidence,
    ) -> MemoryLifecycleStage {
        let age_days = (Utc::now() - evidence.first_seen).num_days();

        match current_stage {
            MemoryLifecycleStage::Candidate => {
                if evidence.frequency >= 2 && evidence.confidence >= 0.6 {
                    MemoryLifecycleStage::Verified
                } else {
                    MemoryLifecycleStage::Candidate
                }
            }
            MemoryLifecycleStage::Verified => {
                if evidence.frequency >= self.min_frequency
                    && evidence.confidence >= self.min_confidence
                    && age_days >= self.min_age_days
                {
                    MemoryLifecycleStage::Permanent
                } else {
                    MemoryLifecycleStage::Verified
                }
            }
            MemoryLifecycleStage::Permanent => {
                // Decay logic could archive it if not seen for a long time
                let days_since_last_seen = (Utc::now() - evidence.last_seen).num_days();
                if days_since_last_seen > 90 {
                    MemoryLifecycleStage::Archived
                } else {
                    MemoryLifecycleStage::Permanent
                }
            }
            MemoryLifecycleStage::Archived => MemoryLifecycleStage::Archived,
        }
    }
}
