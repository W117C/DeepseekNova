use super::evidence::MemoryEvidence;
use super::lifecycle::MemoryLifecycleStage;
use chrono::Utc;

pub struct MemoryPromotionPolicy {
    pub min_frequency: u32,
    pub min_confidence: f64,
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
    pub fn evaluate(&self, current_stage: &MemoryLifecycleStage, evidence: &MemoryEvidence) -> MemoryLifecycleStage {
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
