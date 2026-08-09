use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// 记忆条目的证据聚合：频次、置信度与多源观察时间窗。
///
/// 由晋升策略（[`super::policy::MemoryPromotionPolicy`]）消费，驱动
/// Candidate → Verified → Permanent 的阶段迁移。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEvidence {
    /// 关联的记忆条目 id。
    pub memory_id: String,
    /// 该记忆被观察到的累计次数。
    pub frequency: u32,
    /// 综合置信度 [0.0, 1.0]：多次观察按移动平均聚合。
    pub confidence: f64,
    /// 首次观察到的时间（UTC）。
    pub first_seen: DateTime<Utc>,
    /// 最近一次观察到的时间（UTC）。
    pub last_seen: DateTime<Utc>,
    /// 观察来源的去重计数。
    pub source_count: u32,
}

impl MemoryEvidence {
    /// 创建初始证据：频次/来源计数为 1、置信度 0.5、时间窗为当前时刻。
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

    /// 记录一次新的观察：频次与来源计数自增、刷新 last_seen、置信度按移动平均融合。
    pub fn record_occurrence(&mut self, new_confidence: f64) {
        self.frequency += 1;
        self.last_seen = Utc::now();
        // Moving average or max confidence
        self.confidence = (self.confidence + new_confidence) / 2.0;
        self.source_count += 1;
    }
}
