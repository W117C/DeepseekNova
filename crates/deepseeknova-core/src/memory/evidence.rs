use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEvidence {
    pub memory_id: String,
    pub frequency: u32,
    pub confidence: f64,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub source_count: u32,
}

impl MemoryEvidence {
    pub fn new(memory_id: String) -> Self {
        Self {
            memory_id,
            frequency: 1,
            confidence: 0.5,
            first_seen: Utc::now(),
            last_seen: Utc::now(),
            source_count: 1,
        }
    }

    pub fn record_occurrence(&mut self, new_confidence: f64) {
        self.frequency += 1;
        self.last_seen = Utc::now();
        // Moving average or max confidence
        self.confidence = (self.confidence + new_confidence) / 2.0;
        self.source_count += 1;
    }
}
