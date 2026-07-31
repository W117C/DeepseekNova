//! # MemoryEngine — 统一记忆门面（P1）
//!
//! 把持久 `MemoryStore`（FTS5 + lifecycle）包成运行路径与 CLI 共用的门面：
//! - `recall`：检索并记命中率 + 逐条晋级；
//! - `remember`/`forget`：显式记忆工具后端（id=key 确定性 upsert）；
//! - `record_task`：结束时启发式经验捕获（护栏 + 成本上限 + 脱敏 + 去重）；
//! - `stats`：可观测性快照，驱动 P2 决策。

use crate::memory::redact::redact;
use crate::memory::skill::{TaskObservation, TaskOutcome};
use crate::memory::store::{
    make_entry, MemoryCategory, MemoryEntry, MemorySearchResult, MemoryStore,
};
use anyhow::Result;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tracing::{info, warn};

/// 沉淀护栏（由 runtime 从 `[memory]` 配置装填）。
#[derive(Debug, Clone)]
pub struct DistillGuards {
    pub auto_learn: bool,
    pub min_tool_calls: usize,
    pub min_steps: usize,
    pub max_per_day: u32,
    pub max_per_session: u32,
}

/// 统计快照（CLI `memory stats`；P2 决策依据）。
#[derive(Debug, Clone)]
pub struct MemoryStats {
    pub total: usize,
    pub recall_hit_rate: f64,
    pub reinforce_ratio: f64,
}

/// 统一记忆引擎（P1：FTS5 + lifecycle 持久化）。
pub struct MemoryEngine {
    store: Arc<MemoryStore>,
    redact_secrets: bool,
    session_distills: AtomicU32,
}

/// 由内容派生稳定 id，实现"相同内容不重复插入"的确定性去重。
fn content_id(prefix: &str, content: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut h);
    format!("{prefix}-{:016x}", h.finish())
}

impl MemoryEngine {
    /// 打开磁盘记忆库。
    pub fn open(path: impl AsRef<Path>, redact_secrets: bool) -> Result<Self> {
        Ok(Self {
            store: Arc::new(MemoryStore::open(path.as_ref())?),
            redact_secrets,
            session_distills: AtomicU32::new(0),
        })
    }

    /// 内存库（测试 / 降级用；非 cfg(test)，跨 crate 可用）。
    pub fn open_in_memory(redact_secrets: bool) -> Result<Self> {
        Ok(Self {
            store: Arc::new(MemoryStore::open_in_memory()?),
            redact_secrets,
            session_distills: AtomicU32::new(0),
        })
    }

    /// 召回：记命中率 + 对每条命中执行 record_recall（晋级）。
    pub fn recall(&self, query: &str, top_k: usize) -> Result<Vec<MemorySearchResult>> {
        let results = self.store.search(query, top_k)?;
        self.store.note_recall(!results.is_empty()).ok();
        for r in &results {
            self.store.record_recall(&r.entry.id).ok();
        }
        Ok(results)
    }

    /// 显式记住（id=key 确定性 upsert）。返回 key 是否已存在。
    pub fn remember(&self, key: &str, value: &str, tags: Vec<String>) -> Result<bool> {
        let existed = self.store.meta(key)?.is_some();
        let content = if self.redact_secrets {
            redact(value)
        } else {
            value.to_string()
        };
        let mut entry = make_entry(content, MemoryCategory::Task, tags, "remember-tool", 0.6);
        entry.id = key.to_string();
        self.store.store(&entry)?;
        Ok(existed)
    }

    /// 删除一条记忆。
    pub fn forget(&self, key: &str) -> Result<bool> {
        self.store.delete(key)
    }

    /// 结束时启发式经验捕获（P1，无 LLM）。返回是否实际写入。
    pub fn record_task(&self, obs: &TaskObservation, g: &DistillGuards) -> Result<bool> {
        if !g.auto_learn {
            return Ok(false);
        }
        if obs.tool_calls.len() < g.min_tool_calls || obs.steps_taken.len() < g.min_steps {
            return Ok(false);
        }
        if self.store.distill_count_today()? >= g.max_per_day {
            warn!("distill skipped: daily cap reached");
            return Ok(false);
        }
        // 会话硬上限：原子预留槽位，并发下也不会超额（check-then-act 竞态消除）。
        if self
            .session_distills
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                (cur < g.max_per_session).then_some(cur + 1)
            })
            .is_err()
        {
            warn!("distill skipped: session cap reached");
            return Ok(false);
        }

        let redact_one = |s: &str| -> String {
            if self.redact_secrets {
                redact(s)
            } else {
                s.to_string()
            }
        };

        // 1) 任务摘要（Task 记忆），内容哈希 id 去重
        let summary = redact_one(&format!(
            "Task: {}\nTools used: {}\nOutcome: {:?}",
            obs.task_description,
            obs.tool_calls.join(", "),
            obs.outcome
        ));
        let mut e = make_entry(
            summary.clone(),
            MemoryCategory::Task,
            vec!["task".into(), "summary".into()],
            "auto-distill",
            0.6,
        );
        e.id = content_id("task", &summary);
        self.store.store(&e)?;

        // 2) 失败教训（Skill 记忆，tags=[failure,lesson]）——"自我总结错误形成经验"
        if obs.outcome == TaskOutcome::Failure {
            if let Some(detail) = &obs.user_feedback {
                let lesson = redact_one(&format!(
                    "[lesson] task '{}' failed: {}",
                    obs.task_description, detail
                ));
                let mut le = make_entry(
                    lesson.clone(),
                    MemoryCategory::Skill,
                    vec!["failure".into(), "lesson".into()],
                    "auto-distill",
                    0.8,
                );
                le.id = content_id("lesson", &lesson);
                self.store.store(&le)?;
            }
        }

        self.store.bump_distill_count()?;
        info!(outcome = ?obs.outcome, "task experience captured");
        Ok(true)
    }

    /// 可观测性快照。
    pub fn stats(&self) -> Result<MemoryStats> {
        let (calls, nonempty) = self.store.recall_counters()?;
        Ok(MemoryStats {
            total: self.store.count()?,
            recall_hit_rate: if calls == 0 {
                0.0
            } else {
                nonempty as f64 / calls as f64
            },
            reinforce_ratio: self.store.reinforce_ratio()?,
        })
    }

    /// 泛化计数器 +1（审查指标 review_triggered/issues_found/fix_succeeded 等）。
    pub fn bump_counter(&self, name: &str) -> Result<()> {
        self.store.bump_counter(name)
    }

    /// 读取泛化计数器（缺失 = 0）。
    pub fn read_counter(&self, name: &str) -> Result<u64> {
        self.store.read_counter(name)
    }

    /// 列出某类记忆（CLI）。
    pub fn list(&self, category: MemoryCategory) -> Result<Vec<MemoryEntry>> {
        self.store.list_category(category)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(
        tools: usize,
        steps: usize,
        outcome: TaskOutcome,
        feedback: Option<&str>,
    ) -> TaskObservation {
        TaskObservation {
            task_description: "build a web server".into(),
            tool_calls: vec!["write_file".into(); tools],
            steps_taken: vec!["s".into(); steps],
            outcome,
            user_feedback: feedback.map(|s| s.to_string()),
            session_id: "sess".into(),
        }
    }

    fn guards() -> DistillGuards {
        DistillGuards {
            auto_learn: true,
            min_tool_calls: 5,
            min_steps: 3,
            max_per_day: 50,
            max_per_session: 10,
        }
    }

    #[test]
    fn remember_then_recall_finds_it() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        assert!(!eng.remember("greeting", "hello from rust", vec![]).unwrap());
        let hits = eng.recall("rust", 5).unwrap();
        assert!(hits.iter().any(|h| h.entry.id == "greeting"));
        // 二次 remember 同 key = 已存在
        assert!(eng.remember("greeting", "updated", vec![]).unwrap());
    }

    #[test]
    fn remember_redacts_secrets() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        eng.remember("cred", "API_KEY=sk-ABCD1234efgh5678", vec![])
            .unwrap();
        let hits = eng.recall("API_KEY", 5).unwrap();
        assert!(hits
            .iter()
            .all(|h| !h.entry.content.contains("sk-ABCD1234efgh5678")));
    }

    #[test]
    fn record_task_gated_below_threshold() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        // 工具/步数不够 → 不写
        assert!(!eng
            .record_task(&obs(2, 1, TaskOutcome::Success, None), &guards())
            .unwrap());
    }

    #[test]
    fn record_task_writes_summary_when_eligible() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        assert!(eng
            .record_task(&obs(6, 4, TaskOutcome::Success, None), &guards())
            .unwrap());
        let hits = eng.recall("web server", 5).unwrap();
        assert!(hits.iter().any(|h| h.entry.source == "auto-distill"));
    }

    #[test]
    fn record_task_writes_failure_lesson() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        eng.record_task(
            &obs(6, 4, TaskOutcome::Failure, Some("compile error E0433")),
            &guards(),
        )
        .unwrap();
        let hits = eng.recall("lesson failed", 10).unwrap();
        assert!(hits
            .iter()
            .any(|h| h.entry.tags.contains(&"lesson".to_string())));
    }

    #[test]
    fn record_task_dedups_identical_summary() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        let o = obs(6, 4, TaskOutcome::Success, None);
        eng.record_task(&o, &guards()).unwrap();
        eng.record_task(&o, &guards()).unwrap();
        // 相同摘要内容 → 同 id → upsert，不重复
        let count = eng.list(MemoryCategory::Task).unwrap().len();
        assert_eq!(count, 1, "identical summary must not duplicate");
    }

    #[test]
    fn record_task_respects_session_cap() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        let mut g = guards();
        g.max_per_session = 1;
        let mut o1 = obs(6, 4, TaskOutcome::Success, None);
        o1.task_description = "first task".into();
        let mut o2 = obs(6, 4, TaskOutcome::Success, None);
        o2.task_description = "second task".into();
        assert!(eng.record_task(&o1, &g).unwrap());
        assert!(!eng.record_task(&o2, &g).unwrap(), "session cap must block");
    }

    #[test]
    fn stats_reports_hit_rate() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        eng.remember("k", "rust systems programming", vec![])
            .unwrap();
        eng.recall("rust", 5).unwrap(); // nonempty
        eng.recall("zzz-nomatch", 5).unwrap(); // empty
        let s = eng.stats().unwrap();
        assert!(
            (s.recall_hit_rate - 0.5).abs() < 1e-9,
            "got {}",
            s.recall_hit_rate
        );
    }
}
