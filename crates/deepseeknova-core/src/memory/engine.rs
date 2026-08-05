//! # MemoryEngine — 统一记忆门面（P1）
//!
//! 把持久 `MemoryStore`（FTS5 + lifecycle）包成运行路径与 CLI 共用的门面：
//! - `recall`：检索并记命中率 + 逐条晋级；
//! - `remember`/`forget`：显式记忆工具后端（id=key 确定性 upsert）；
//! - `record_task`：结束时启发式经验捕获（护栏 + 成本上限 + 脱敏 + 去重）；
//! - `stats`：可观测性快照，驱动 P2 决策。

use crate::memory::lifecycle::MemoryLifecycleStage;
use crate::memory::redact::redact;
use crate::memory::skill::{TaskObservation, TaskOutcome};
use crate::memory::store::{
    make_entry, MemoryCategory, MemoryEntry, MemorySearchResult, MemoryStore, DEFAULT_RANK_WEIGHT,
};
use anyhow::Result;
use chrono::Utc;
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
    /// stage 分布（真实条目；含 archived，按 stage 名排序）。
    pub stage_counts: Vec<(String, usize)>,
    /// archived 条目数。
    pub archived: usize,
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
    /// 排序融合生命周期因子，权重为配置默认（[`DEFAULT_RANK_WEIGHT`]）。
    pub fn recall(&self, query: &str, top_k: usize) -> Result<Vec<MemorySearchResult>> {
        self.recall_with_weight(query, top_k, DEFAULT_RANK_WEIGHT)
    }

    /// 带生命周期排序权重的召回（权重 0 = 纯 bm25 等价旧行为）。
    /// CLI 等入口把 `[memory] rank_lifecycle_weight` 传进来。
    pub fn recall_with_weight(
        &self,
        query: &str,
        top_k: usize,
        rank_weight: f64,
    ) -> Result<Vec<MemorySearchResult>> {
        let results = self.store.search_with_weight(query, top_k, rank_weight)?;
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

    /// 反思闭环的教训落库（薄封装：统一走 [`Self::record_knowledge`]）。
    /// content="kind: lesson\n{lesson}"、id 前缀 "distill"（与 LLM 蒸馏跨入口去重）、
    /// category=Skill、source=reflect-loop、tags=[reflect, lesson]。返回是否新写入。
    pub fn record_reflection_lesson(&self, lesson: &str) -> Result<bool> {
        self.record_knowledge(
            "lesson",
            "",
            lesson,
            vec!["reflect".to_string(), "lesson".to_string()],
            "reflect-loop",
        )
    }

    /// 删除一条记忆。
    pub fn forget(&self, key: &str) -> Result<bool> {
        self.store.delete(key)
    }

    /// 衰减一轮：非 permanent 记忆 importance -= decay_rate * recency_bonus，
    /// importance < 0.1 → archived；permanent 豁免。返回发生衰减的条数。
    /// 由 `memory cleanup` 显式触发（不自动跑，避免运行期不可预期写放大）。
    pub fn decay(&self, decay_rate: f32) -> Result<usize> {
        let mut decayed = 0;
        for (id, mut meta) in self.store.all_lifecycle()? {
            if meta.stage == MemoryLifecycleStage::Permanent {
                continue;
            }
            let before = meta.importance;
            meta.apply_decay(decay_rate);
            if meta.importance < before {
                self.store.update_lifecycle(&id, &meta)?;
                decayed += 1;
            }
        }
        Ok(decayed)
    }

    /// 清理闭环：先衰减一轮（decay_rate），再删除 archived 且距最后召回
    /// （无召回按创建时间）超过 archive_ttl_days 的记忆。返回 (decayed, deleted)。
    pub fn cleanup(&self, decay_rate: f32, archive_ttl_days: u32) -> Result<(usize, usize)> {
        let decayed = self.decay(decay_rate)?;
        let cutoff = Utc::now().timestamp() - i64::from(archive_ttl_days) * 86_400;
        let deleted = self.store.delete_archived_older_than(cutoff)?;
        Ok((decayed, deleted))
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

        // 3) 任务-文件关联（P3.3）：为每个触碰文件写入确定性 file-link 记忆，
        // 让后续"这个文件相关的经验"检索能命中。
        for f in obs.files.iter().take(20) {
            let f = redact_one(f);
            if f.trim().is_empty() {
                continue;
            }
            let link = redact_one(&format!("file: {f}\nTask: {}", obs.task_description));
            let mut fe = make_entry(
                link.clone(),
                MemoryCategory::Task,
                vec!["file-link".into(), "task".into()],
                "auto-distill",
                0.5,
            );
            fe.id = content_id("file", &f);
            self.store.store(&fe)?;
        }

        self.store.bump_distill_count()?;
        info!(outcome = ?obs.outcome, "task experience captured");
        Ok(true)
    }

    /// 可观测性快照。
    pub fn stats(&self) -> Result<MemoryStats> {
        let (calls, nonempty) = self.store.recall_counters()?;
        let stage_counts = self.store.stage_counts()?;
        let archived = stage_counts
            .iter()
            .find(|(s, _)| s == "archived")
            .map(|(_, n)| *n)
            .unwrap_or(0);
        Ok(MemoryStats {
            total: self.store.count()?,
            recall_hit_rate: if calls == 0 {
                0.0
            } else {
                nonempty as f64 / calls as f64
            },
            reinforce_ratio: self.store.reinforce_ratio()?,
            stage_counts,
            archived,
        })
    }

    /// 统一蒸馏入口：kind ∈ skill|lesson；title 为空时省略 title 行
    /// （content = "kind: {kind}\n{body}"，与反思 lesson 格式一致）；
    /// content 哈希去重 + 脱敏；category=Skill、importance=0.8、id 前缀统一 "distill"
    /// （跨入口同内容去重）。tags/source 由调用方决定。返回是否新写入。
    pub fn record_knowledge(
        &self,
        kind: &str,
        title: &str,
        body: &str,
        tags: Vec<String>,
        source: &str,
    ) -> Result<bool> {
        let raw = if title.is_empty() {
            format!("kind: {kind}\n{body}")
        } else {
            format!("kind: {kind}\ntitle: {title}\n{body}")
        };
        let content = if self.redact_secrets {
            redact(&raw)
        } else {
            raw
        };
        let mut e = make_entry(content.clone(), MemoryCategory::Skill, tags, source, 0.8);
        e.id = content_id("distill", &content);
        let existed = self.store.meta(&e.id)?.is_some();
        if existed {
            // 同内容去重：不重复写入，保留首次入库的 tags/source。
            return Ok(false);
        }
        self.store.store(&e)?;
        Ok(true)
    }

    /// LLM 蒸馏知识落库（薄封装：统一走 [`Self::record_knowledge`]）。
    /// kind ∈ skill|lesson；content 哈希去重 + 脱敏；category=Skill、
    /// source=llm-distill、tags 追加 llm-distill。返回是否新写入（同内容二次写入返回 false）。
    pub fn record_llm_knowledge(
        &self,
        kind: &str,
        title: &str,
        body: &str,
        mut tags: Vec<String>,
    ) -> Result<bool> {
        if !tags.iter().any(|t| t == "llm-distill") {
            tags.push("llm-distill".to_string());
        }
        self.record_knowledge(kind, title, body, tags, "llm-distill")
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
    use crate::memory::lifecycle::LifecycleMeta;

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
            files: vec![],
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
    fn record_task_writes_deduped_file_links() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        let mut o = obs(6, 4, TaskOutcome::Success, None);
        o.files = vec![
            "src/main.rs".into(),
            "src/lib.rs".into(),
            "src/main.rs".into(),
        ];
        eng.record_task(&o, &guards()).unwrap();
        let hits = eng.recall("file: src/main.rs", 10).unwrap();
        assert!(
            hits.iter()
                .any(|h| h.entry.tags.contains(&"file-link".to_string())),
            "file-link memory must be searchable"
        );
        let links: Vec<_> = eng
            .list(MemoryCategory::Task)
            .unwrap()
            .into_iter()
            .filter(|e| e.tags.contains(&"file-link".to_string()))
            .collect();
        assert_eq!(links.len(), 2, "distinct files deduped");
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

    #[test]
    fn llm_knowledge_stores_skill_and_dedupes() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        assert!(eng
            .record_llm_knowledge(
                "skill",
                "Use serde derive",
                "Prefer derive over manual impls",
                vec!["serde".into()],
            )
            .unwrap());
        let skills = eng.list(MemoryCategory::Skill).unwrap();
        assert_eq!(skills.len(), 1);
        assert!(skills[0].content.contains("Use serde derive"));
        assert!(skills[0].tags.contains(&"llm-distill".to_string()));
        assert_eq!(skills[0].source, "llm-distill");

        // 同内容二次写入不重复
        assert!(!eng
            .record_llm_knowledge(
                "skill",
                "Use serde derive",
                "Prefer derive over manual impls",
                vec!["serde".into()],
            )
            .unwrap());
        assert_eq!(eng.list(MemoryCategory::Skill).unwrap().len(), 1);
    }

    #[test]
    fn reflection_lesson_stores_and_dedupes() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        assert!(eng
            .record_reflection_lesson("always escape user input")
            .unwrap());
        let skills = eng.list(MemoryCategory::Skill).unwrap();
        assert_eq!(skills.len(), 1);
        assert!(skills[0].content.contains("always escape user input"));
        assert!(skills[0].tags.contains(&"reflect".to_string()));
        assert!(skills[0].tags.contains(&"lesson".to_string()));
        assert_eq!(skills[0].source, "reflect-loop");

        assert!(!eng
            .record_reflection_lesson("always escape user input")
            .unwrap());
        assert_eq!(eng.list(MemoryCategory::Skill).unwrap().len(), 1);
    }

    #[test]
    fn llm_knowledge_redacts_secrets() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        eng.record_llm_knowledge(
            "lesson",
            "api key leak",
            "API_KEY=sk-ABCD1234efgh5678 in logs",
            vec![],
        )
        .unwrap();
        let skills = eng.list(MemoryCategory::Skill).unwrap();
        assert!(
            !skills[0].content.contains("sk-ABCD1234efgh5678"),
            "秘密必须被脱敏：{}",
            skills[0].content
        );
    }

    #[test]
    fn reflection_lesson_redacts_secrets() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        eng.record_reflection_lesson("never log API_KEY=sk-ABCD1234efgh5678")
            .unwrap();
        let skills = eng.list(MemoryCategory::Skill).unwrap();
        assert!(
            !skills[0].content.contains("sk-ABCD1234efgh5678"),
            "秘密必须被脱敏：{}",
            skills[0].content
        );
    }

    #[test]
    fn decay_reduces_importance_and_exempts_permanent() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        eng.remember("normal", "rust borrow checker", vec![])
            .unwrap();
        eng.remember("perm", "rust lifetime design", vec![])
            .unwrap();
        // 把 perm 提升为 permanent（模拟多次召回晋级后的状态）。
        let meta = LifecycleMeta {
            stage: MemoryLifecycleStage::Permanent,
            recall_count: 3,
            last_recalled_at: None,
            created_at: Utc::now().timestamp(),
            importance: 0.9,
        };
        eng.store.update_lifecycle("perm", &meta).unwrap();

        let decayed = eng.decay(1.0).unwrap();
        assert_eq!(decayed, 1, "仅非 permanent 条目计入衰减");
        let lc: Vec<_> = eng.store.all_lifecycle().unwrap();
        let normal = lc.iter().find(|(id, _)| id == "normal").unwrap();
        assert!(normal.1.importance < 0.5, "importance 必须下降");
        let perm = lc.iter().find(|(id, _)| id == "perm").unwrap();
        assert_eq!(perm.1.importance, 0.9, "permanent 豁免衰减");
        assert_eq!(perm.1.stage.as_str(), "permanent");
    }

    #[test]
    fn decay_archives_below_threshold() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        eng.remember("low", "obsolete note", vec![]).unwrap();
        // 初始 importance 0.6；先衰减到 <0.1 触发归档。
        let decayed = eng.decay(1.0).unwrap();
        assert_eq!(decayed, 1);
        let lc: Vec<_> = eng.store.all_lifecycle().unwrap();
        let low = lc.iter().find(|(id, _)| id == "low").unwrap();
        assert_eq!(
            low.1.stage.as_str(),
            "archived",
            "importance < 0.1 必须归档"
        );
        assert_eq!(low.1.importance, 0.0);
    }

    #[test]
    fn cleanup_deletes_expired_archived_only() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        eng.remember("fresh", "active note", vec![]).unwrap(); // decay 后 archived 但创建于现在
        eng.remember("expired", "old note", vec![]).unwrap();
        eng.remember("recently", "recent archived note", vec![])
            .unwrap();
        let now = Utc::now().timestamp();
        // expired：archived 且最后召回在 40 天前 → 应删除。
        eng.store
            .update_lifecycle(
                "expired",
                &LifecycleMeta {
                    stage: MemoryLifecycleStage::Archived,
                    recall_count: 0,
                    last_recalled_at: Some(now - 40 * 86_400),
                    created_at: now - 60 * 86_400,
                    importance: 0.2,
                },
            )
            .unwrap();
        // recently：archived 但最后召回在 5 天前 → 保留。
        eng.store
            .update_lifecycle(
                "recently",
                &LifecycleMeta {
                    stage: MemoryLifecycleStage::Archived,
                    recall_count: 0,
                    last_recalled_at: Some(now - 5 * 86_400),
                    created_at: now - 10 * 86_400,
                    importance: 0.2,
                },
            )
            .unwrap();

        let (decayed, deleted) = eng.cleanup(1.0, 30).unwrap();
        assert_eq!(deleted, 1, "仅删除超期 archived");
        assert!(decayed >= 3, "衰减覆盖全部非 permanent 条目");
        let ids: Vec<String> = eng
            .list(MemoryCategory::Task)
            .unwrap()
            .into_iter()
            .map(|e| e.id)
            .collect();
        assert!(!ids.contains(&"expired".to_string()), "超期条目已删除");
        assert!(ids.contains(&"fresh".to_string()), "未过期条目保留");
        assert!(
            ids.contains(&"recently".to_string()),
            "近期召回 archived 保留"
        );
    }

    #[test]
    fn stats_reports_stage_distribution() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        eng.remember("c1", "candidate note", vec![]).unwrap();
        eng.remember("v1", "verified note", vec![]).unwrap();
        eng.remember("a1", "archived note", vec![]).unwrap();
        eng.store.record_recall("v1").unwrap();
        let now = Utc::now().timestamp();
        eng.store
            .update_lifecycle(
                "a1",
                &LifecycleMeta {
                    stage: MemoryLifecycleStage::Archived,
                    recall_count: 0,
                    last_recalled_at: None,
                    created_at: now - 100 * 86_400,
                    importance: 0.05,
                },
            )
            .unwrap();
        let s = eng.stats().unwrap();
        let get = |stage: &str| {
            s.stage_counts
                .iter()
                .find(|(k, _)| k == stage)
                .map(|(_, n)| *n)
                .unwrap_or(0)
        };
        assert_eq!(s.total, 3);
        assert_eq!(get("candidate"), 1);
        assert_eq!(get("verified"), 1);
        assert_eq!(get("archived"), 1);
        assert_eq!(s.archived, 1);
    }

    #[test]
    fn knowledge_dedupes_across_entrances() {
        // 统一蒸馏入口：reflect 先写入后，llm-distill 同内容不重复。
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        assert!(eng
            .record_reflection_lesson("always escape user input")
            .unwrap());
        assert!(
            !eng.record_llm_knowledge("lesson", "", "always escape user input", vec![])
                .unwrap(),
            "跨入口同内容必须去重"
        );
        let skills = eng.list(MemoryCategory::Skill).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].content, "kind: lesson\nalways escape user input");
        assert_eq!(skills[0].source, "reflect-loop");

        // 两入口产出同格式（带 title 的 llm 入口 + 无 title 的 reflect 入口）。
        let eng2 = MemoryEngine::open_in_memory(true).unwrap();
        assert!(eng2
            .record_llm_knowledge("skill", "Use serde derive", "Prefer derive", vec![])
            .unwrap());
        assert!(!eng2
            .record_llm_knowledge("skill", "Use serde derive", "Prefer derive", vec![])
            .unwrap());
        let skills2 = eng2.list(MemoryCategory::Skill).unwrap();
        assert_eq!(skills2.len(), 1);
        assert_eq!(
            skills2[0].content,
            "kind: skill\ntitle: Use serde derive\nPrefer derive"
        );
        assert!(skills2[0].tags.contains(&"llm-distill".to_string()));
    }
}
