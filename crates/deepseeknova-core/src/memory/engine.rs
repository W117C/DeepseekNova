//! # MemoryEngine — 统一记忆门面（P1）
//!
//! 把持久 `MemoryStore`（FTS5 + lifecycle）包成运行路径与 CLI 共用的门面：
//! - `recall`：检索并记命中率 + 逐条晋级；
//! - `remember`/`forget`：显式记忆工具后端（id=key 确定性 upsert）；
//! - `record_task`：结束时启发式经验捕获（护栏 + 成本上限 + 脱敏 + 去重）；
//! - `stats`：可观测性快照，驱动 P2 决策。

use crate::memory::embedding::EmbeddingProvider;
use crate::memory::lifecycle::{LifecycleMeta, MemoryLifecycleStage};
use crate::memory::redact::redact;
use crate::memory::skill::{TaskObservation, TaskOutcome};
use crate::memory::store::{
    make_entry, MemoryCategory, MemoryEntry, MemoryScoreBreakdown, MemorySearchResult, MemoryStore,
    DEFAULT_RANK_WEIGHT,
};
use crate::DeepseeknovaError;
use chrono::Utc;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tracing::{info, warn};

/// 模块内 Result 简写：默认错误类型为 [`DeepseeknovaError`]。
type Result<T> = std::result::Result<T, DeepseeknovaError>;

/// 沉淀护栏（由 runtime 从 `[memory]` 配置装填）。
#[derive(Debug, Clone)]
pub struct DistillGuards {
    /// 是否启用自动经验沉淀（false 时所有 distill 入口短路）。
    pub auto_learn: bool,
    /// 触发沉淀所需的最小工具调用次数。
    pub min_tool_calls: usize,
    /// 触发沉淀所需的最小执行步数。
    pub min_steps: usize,
    /// 每日沉淀次数上限（UTC 日）。
    pub max_per_day: u32,
    /// 单会话沉淀次数上限（原子预留槽位，并发安全）。
    pub max_per_session: u32,
}

/// 统计快照（CLI `memory stats`；P2 决策依据）。
#[derive(Debug, Clone)]
pub struct MemoryStats {
    /// 记忆库总条目数。
    pub total: usize,
    /// 已有嵌入向量的条目数（覆盖率；embedder=none 时恒 0）。
    pub embedded: usize,
    /// 召回命中率（recall_nonempty / recall_calls；零调用时为 0.0）。
    pub recall_hit_rate: f64,
    /// auto-distill 来源条目中已达 verified/permanent 的比例。
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
    embedder: Option<Arc<dyn EmbeddingProvider>>,
    embed_model: Option<String>,
    session_distills: AtomicU32,
}

/// 由内容派生稳定 id，实现"相同内容不重复插入"的确定性去重。
///
/// 基于 SHA-256（而非 `DefaultHasher`）：后者算法未承诺跨 Rust 编译版本稳定，
/// 编译器升级后旧库条目的去重 id（distill-*/lesson-*/file-*）将无法再命中，
/// 产生重复记忆且跨入口去重失效。SHA-256 是密码学标准哈希，跨版本/跨平台稳定。
/// id 格式为 `{prefix}-{Sha256(content) hex 前 16 位}`：长度语义与原
/// DefaultHasher(u64) 的 16 位 hex 输出保持一致（64 bit），前缀不变。
fn content_id(prefix: &str, content: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = hex::encode(Sha256::digest(content.as_bytes()));
    format!("{prefix}-{}", &digest[..16])
}

impl MemoryEngine {
    /// 打开磁盘记忆库。
    pub fn open(path: impl AsRef<Path>, redact_secrets: bool) -> Result<Self> {
        Self::open_with_embedder(path, redact_secrets, None, None)
    }

    /// 打开磁盘记忆库并装配可选嵌入后端（embedder=remote 时由 runtime/CLI 传入）。
    pub fn open_with_embedder(
        path: impl AsRef<Path>,
        redact_secrets: bool,
        embedder: Option<Arc<dyn EmbeddingProvider>>,
        embed_model: Option<String>,
    ) -> Result<Self> {
        Ok(Self {
            store: Arc::new(MemoryStore::open(path.as_ref())?),
            redact_secrets,
            embedder,
            embed_model,
            session_distills: AtomicU32::new(0),
        })
    }

    /// 内存库（测试 / 降级用；非 cfg(test)，跨 crate 可用）。
    pub fn open_in_memory(redact_secrets: bool) -> Result<Self> {
        Self::open_in_memory_with_embedder(redact_secrets, None, None)
    }

    /// 内存库 + 可选嵌入后端（测试 / 降级用）。
    pub fn open_in_memory_with_embedder(
        redact_secrets: bool,
        embedder: Option<Arc<dyn EmbeddingProvider>>,
        embed_model: Option<String>,
    ) -> Result<Self> {
        Ok(Self {
            store: Arc::new(MemoryStore::open_in_memory()?),
            redact_secrets,
            embedder,
            embed_model,
            session_distills: AtomicU32::new(0),
        })
    }

    /// 尽力而为：为单条记忆生成并持久化嵌入。同模型已有向量则跳过；
    /// provider 缺失/失败只 warn，绝不回滚写入或打断调用方（fail-open）。
    fn embed_entry(&self, id: &str, content: &str) {
        let (Some(_), Some(model)) = (&self.embedder, &self.embed_model) else {
            return;
        };
        if let Ok(Some((_, existing))) = self.store.get_embedding(id) {
            if existing == *model {
                return;
            }
        }
        self.reembed_entry(id, content);
    }

    /// 无条件重算嵌入（区别于 [`Self::embed_entry`]：同模型已有向量也会刷新）。
    /// 供 `edit` 使用——内容变更后旧向量已失配，必须强制覆盖。
    fn reembed_entry(&self, id: &str, content: &str) {
        let (Some(p), Some(model)) = (&self.embedder, &self.embed_model) else {
            return;
        };
        match p.embed(content) {
            Ok(v) => {
                if let Err(e) = self.store.upsert_embedding(id, &v, model) {
                    warn!(id, error = %e, "memory embedding persist failed");
                }
            }
            Err(e) => warn!(id, error = %e, "memory embedding failed; FTS-only fallback"),
        }
    }

    /// 为尚无向量的旧记忆显式回填嵌入（跳过 archived；无 provider 时无操作）。
    /// 返回 (尝试条数, 成功条数)。
    pub fn backfill_embeddings(&self) -> Result<(usize, usize)> {
        if self.embedder.is_none() || self.embed_model.is_none() {
            return Ok((0, 0));
        }
        let pending = self.store.entries_without_embedding()?;
        let mut ok = 0usize;
        for (id, content) in &pending {
            self.embed_entry(id, content);
            if self.store.get_embedding(id)?.is_some() {
                ok += 1;
            }
        }
        Ok((pending.len(), ok))
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
        let results = match (&self.embedder, &self.embed_model) {
            (Some(p), Some(m)) => self.store.search_hybrid_with_weight(
                query,
                top_k,
                Some(p.as_ref()),
                m,
                rank_weight,
            )?,
            _ => self.store.search_with_weight(query, top_k, rank_weight)?,
        };
        self.store.note_recall(!results.is_empty()).ok();
        for r in &results {
            self.store.record_recall(&r.entry.id).ok();
        }
        Ok(results)
    }

    /// 召回回放（P1-11：CLI `memory replay` 后端）：执行与 [`Self::recall_with_weight`]
    /// **完全同源**的检索路径，但额外返回每条命中的分数分解
    /// （bm25 / 余弦 / 生命周期惩罚，见 [`MemoryScoreBreakdown`]）。
    /// 与 recall 不同：**不**记召回命中率、**不**执行 record_recall 晋级——
    /// 回放是只读诊断，"观察"不应改变生命周期状态。
    pub fn replay(
        &self,
        query: &str,
        top_k: usize,
        rank_weight: f64,
    ) -> Result<Vec<MemoryScoreBreakdown>> {
        match (&self.embedder, &self.embed_model) {
            (Some(p), Some(m)) => {
                self.store
                    .search_hybrid_breakdown(query, top_k, Some(p.as_ref()), m, rank_weight)
            }
            _ => self.store.search_breakdown(query, top_k, rank_weight),
        }
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
        self.embed_entry(&entry.id, &entry.content);
        Ok(existed)
    }

    /// 编辑一条记忆的内容（P1-11：CLI `memory edit` 后端）。
    /// 保留 id/tags/source/category/importance 与 lifecycle 元数据
    /// （stage/recall_count/last_recalled_at 不被重置）；内容按
    /// `redact_secrets` 脱敏后写回，并**强制重算**嵌入（若启用）。
    /// 返回 id 是否存在。
    pub fn edit(&self, id: &str, new_content: &str) -> Result<bool> {
        let mut found: Option<MemoryEntry> = None;
        for cat in [
            MemoryCategory::Task,
            MemoryCategory::Skill,
            MemoryCategory::UserProfile,
        ] {
            for e in self.store.list_category(cat)? {
                if e.id == id {
                    found = Some(e);
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        let Some(mut entry) = found else {
            return Ok(false);
        };
        entry.content = if self.redact_secrets {
            redact(new_content)
        } else {
            new_content.to_string()
        };
        // store() 为 id 幂等 upsert：fts/cjk/meta 三表原子更新，
        // meta 行冲突时仅刷新 importance（stage/recall/embedding 保留）。
        self.store.store(&entry)?;
        self.reembed_entry(&entry.id, &entry.content);
        Ok(true)
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
    /// 实现走 store 层事务化批量衰减（单一事务内读-算-写，防并发 record_recall
    /// 增量被覆盖写回冲掉）；decay_rate 在 store 入口 clamp 到 0.0..=1.0。
    pub fn decay(&self, decay_rate: f32) -> Result<usize> {
        self.store.decay_all(decay_rate)
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
        self.embed_entry(&e.id, &e.content);

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
                self.embed_entry(&le.id, &le.content);
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
            self.embed_entry(&fe.id, &fe.content);
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
            embedded: self.store.embedding_count()?,
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
        // 去重同时查新旧两个候选 id：旧库（前缀统一前）的 reflect-<hash> 条目
        // 与新 distill-<hash> 同内容不再互相去重，命中任一即视为已存在。
        let existed = self.store.meta(&e.id)?.is_some()
            || self.store.meta(&content_id("reflect", &content))?.is_some();
        if existed {
            // 同内容去重：不重复写入，保留首次入库的 tags/source。
            return Ok(false);
        }
        self.store.store(&e)?;
        self.embed_entry(&e.id, &e.content);
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

    /// 列出某类记忆并附 lifecycle 元数据（P1-11：CLI `memory list` 展示
    /// stage / importance / recency 用）。meta 缺失的条目以候选身份兜底。
    pub fn list_with_lifecycle(
        &self,
        category: MemoryCategory,
    ) -> Result<Vec<(MemoryEntry, LifecycleMeta)>> {
        let entries = self.store.list_category(category)?;
        let lifecycle: HashMap<String, LifecycleMeta> =
            self.store.all_lifecycle()?.into_iter().collect();
        Ok(entries
            .into_iter()
            .map(|e| {
                let meta = lifecycle.get(&e.id).cloned().unwrap_or(LifecycleMeta {
                    stage: MemoryLifecycleStage::Candidate,
                    recall_count: 0,
                    last_recalled_at: None,
                    created_at: e.created_at,
                    importance: e.importance,
                });
                (e, meta)
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::lifecycle::{LifecycleMeta, MemoryLifecycleStage};

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
    fn knowledge_dedupes_against_legacy_reflect_prefix() {
        // C2：旧库（前缀统一前）中 reflect-<hash> 条目存在时，新 distill-<hash>
        // 同内容写入必须被去重（不写、返回 false）。
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        let content = "kind: lesson\nalways escape user input";
        let legacy_id = content_id("reflect", content);
        let mut legacy = make_entry(
            content.to_string(),
            MemoryCategory::Skill,
            vec!["reflect".into(), "lesson".into()],
            "reflect-loop",
            0.8,
        );
        legacy.id = legacy_id.clone();
        eng.store.store(&legacy).unwrap();
        assert!(
            eng.store.meta(&legacy_id).unwrap().is_some(),
            "旧前缀条目已就位"
        );

        // 新入口写入同内容 → 命中旧前缀候选 id → 去重
        assert!(
            !eng.record_reflection_lesson("always escape user input")
                .unwrap(),
            "旧前缀 reflect-* 存在时新写入必须被去重"
        );
        let skills = eng.list(MemoryCategory::Skill).unwrap();
        assert_eq!(skills.len(), 1, "不得产生重复条目");
        assert_eq!(skills[0].id, legacy_id, "保留首次入库的旧前缀条目");

        // llm 入口同内容同样去重
        assert!(!eng
            .record_llm_knowledge("lesson", "", "always escape user input", vec![])
            .unwrap());
        assert_eq!(eng.list(MemoryCategory::Skill).unwrap().len(), 1);
    }

    #[test]
    fn decay_clamps_rate_to_valid_range() {
        // C6：decay_rate 越界（负值 / >1）必须被 clamp，不得使 importance 上升或超量清零。
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        eng.remember("n1", "negative decay note", vec![]).unwrap();
        // 负值 → clamp 为 0 → 不衰减（importance 不上升、不算 decayed）。
        let decayed = eng.decay(-0.5).unwrap();
        assert_eq!(decayed, 0, "负 decay_rate 不得使 importance 上升");
        let lc: Vec<_> = eng.store.all_lifecycle().unwrap();
        let n1 = lc.iter().find(|(id, _)| id == "n1").unwrap();
        assert_eq!(n1.1.importance, 0.6, "clamp 后 importance 保持不变");

        // >1 → clamp 为 1 → 一次清零（与 1.0 行为一致）。
        let decayed_big = eng.decay(5.0).unwrap();
        assert_eq!(decayed_big, 1);
        let lc: Vec<_> = eng.store.all_lifecycle().unwrap();
        let n1 = lc.iter().find(|(id, _)| id == "n1").unwrap();
        assert_eq!(n1.1.importance, 0.0, ">1 与 1.0 等价：一次清零");
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

    #[test]
    fn content_id_is_sha256_stable_and_distinct() {
        // T17：content_id 必须基于跨编译版本稳定的 SHA-256（而非 DefaultHasher）。
        // 同内容 → 同 id；不同内容 → 不同 id；前缀与 16 位 hex 长度语义不变。
        let a1 = content_id("distill", "always escape user input");
        let a2 = content_id("distill", "always escape user input");
        assert_eq!(a1, a2, "同内容必须产出同 id（去重基础）");

        // 长度/前缀语义：`{prefix}-` + 16 位 hex（与原 u64 hex 输出一致）。
        assert!(a1.starts_with("distill-"), "前缀语义不变");
        assert_eq!(
            a1.len(),
            "distill-".len() + 16,
            "id 保持 16 位 hex 长度语义"
        );
        let hash_part = a1.split_once('-').unwrap().1;
        assert!(
            hash_part.chars().all(|c| c.is_ascii_hexdigit()),
            "hash 部分必须为 hex 字符"
        );

        // 不同内容（仅尾部空格不同）→ 不同 id。
        let b = content_id("distill", "always escape user input ");
        assert_ne!(a1, b, "不同内容必须产出不同 id");

        // 前缀不同但内容相同 → 不同 id（distill / reflect 分属不同去重域）。
        assert_ne!(
            content_id("reflect", "always escape user input"),
            a1,
            "前缀参与 id 派生"
        );
    }

    /// 确定性测试替身（接口替身，非被测对象 mock）：语义命中不需 FTS 共词。
    struct FakeEmbed;

    impl EmbeddingProvider for FakeEmbed {
        fn embed(&self, text: &str) -> Result<Vec<f32>> {
            if text.contains("ferris") {
                Ok(vec![0.9, 0.1])
            } else if text.contains("rust") {
                Ok(vec![1.0, 0.0])
            } else {
                Ok(vec![0.0, 1.0])
            }
        }
    }

    /// 失败替身：验证 fail-open（写入不因嵌入失败而回滚）。
    struct FailingEmbed;

    impl EmbeddingProvider for FailingEmbed {
        fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Err(DeepseeknovaError::provider_retryable("network unavailable"))
        }
    }

    #[test]
    fn write_embeds_and_recall_finds_semantic_match() {
        let eng = MemoryEngine::open_in_memory_with_embedder(
            true,
            Some(Arc::new(FakeEmbed)),
            Some("test-model".to_string()),
        )
        .unwrap();
        eng.remember("k", "ferris crab language", vec![]).unwrap();
        assert!(
            eng.store.get_embedding("k").unwrap().is_some(),
            "remember 后必须自动生成嵌入"
        );
        let hits = eng.recall("rust", 5).unwrap();
        assert_eq!(hits.len(), 1, "语义独有命中必须被召回");
        assert_eq!(hits[0].entry.id, "k");
        assert_eq!(eng.stats().unwrap().embedded, 1);
    }

    #[test]
    fn embedder_failure_keeps_write_and_fts_recall() {
        let eng = MemoryEngine::open_in_memory_with_embedder(
            true,
            Some(Arc::new(FailingEmbed)),
            Some("test-model".to_string()),
        )
        .unwrap();
        assert!(!eng.remember("k", "rust borrow checker", vec![]).unwrap());
        assert!(
            eng.store.get_embedding("k").unwrap().is_none(),
            "嵌入失败不得写入向量"
        );
        let hits = eng.recall("rust", 5).unwrap();
        assert_eq!(hits.len(), 1, "嵌入失败必须回落纯 FTS 召回");
        assert_eq!(hits[0].entry.id, "k");
    }

    #[test]
    fn backfill_embeddings_skips_archived_and_counts() {
        use crate::memory::lifecycle::{LifecycleMeta, MemoryLifecycleStage};
        use crate::memory::store::make_entry;
        let eng = MemoryEngine::open_in_memory_with_embedder(
            true,
            Some(Arc::new(FakeEmbed)),
            Some("test-model".to_string()),
        )
        .unwrap();
        let mut e1 = make_entry(
            "ferris crab language",
            MemoryCategory::Task,
            vec![],
            "t",
            0.5,
        );
        e1.id = "e1".to_string();
        let mut e2 = make_entry(
            "rust borrow checker",
            MemoryCategory::Task,
            vec![],
            "t",
            0.5,
        );
        e2.id = "e2".to_string();
        let mut e3 = make_entry("archived legacy", MemoryCategory::Task, vec![], "t", 0.5);
        e3.id = "e3".to_string();
        eng.store.store(&e1).unwrap();
        eng.store.store(&e2).unwrap();
        eng.store.store(&e3).unwrap();
        let _ = eng.store.update_lifecycle(
            &e3.id,
            &LifecycleMeta {
                stage: MemoryLifecycleStage::Archived,
                recall_count: 0,
                last_recalled_at: None,
                created_at: e3.created_at,
                importance: 0.5,
            },
        );

        let (attempted, ok) = eng.backfill_embeddings().unwrap();
        assert_eq!((attempted, ok), (2, 2), "archived 必须跳过");
        assert_eq!(eng.store.embedding_count().unwrap(), 2);
        assert_eq!(eng.stats().unwrap().embedded, 2);
        // 二次回填应为无操作。
        assert_eq!(eng.backfill_embeddings().unwrap(), (0, 0));
    }

    // ── P1-11 用户面：list/edit/replay ──────────────────────────────────

    #[test]
    fn list_with_lifecycle_includes_stage_and_recency() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        eng.remember("k", "note", vec![]).unwrap();
        eng.recall("note", 5).unwrap(); // 晋级 verified + 记录召回时间
        let items = eng.list_with_lifecycle(MemoryCategory::Task).unwrap();
        assert_eq!(items.len(), 1);
        let (e, meta) = &items[0];
        assert_eq!(e.id, "k");
        assert_eq!(meta.stage.as_str(), "verified");
        assert_eq!(meta.recall_count, 1);
        assert!(
            meta.last_recalled_at.is_some(),
            "recall 后必须记录最近召回时间"
        );
        // 其它类目为空。
        assert!(eng
            .list_with_lifecycle(MemoryCategory::Skill)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn edit_updates_content_and_reembeds() {
        let eng = MemoryEngine::open_in_memory_with_embedder(
            true,
            Some(Arc::new(FakeEmbed)),
            Some("test-model".to_string()),
        )
        .unwrap();
        eng.remember("k", "ferris crab language", vec![]).unwrap();
        // 编辑前：嵌入为 ferris 对应的 [0.9, 0.1]。
        assert!(eng.edit("k", "rust borrow checker").unwrap());
        // 内容已更新。
        let hits = eng.recall("rust", 5).unwrap();
        assert!(
            hits.iter()
                .any(|h| h.entry.id == "k" && h.entry.content.contains("rust borrow")),
            "edit 后新内容必须可召回"
        );
        // 嵌入已强制重算：新内容（rust → [1.0, 0.0]）覆盖旧向量。
        let (vec, model) = eng.store.get_embedding("k").unwrap().expect("嵌入存在");
        assert_eq!(model, "test-model");
        assert!(vec[0] > vec[1], "重算后向量应反映新内容而非旧内容: {vec:?}");
    }

    #[test]
    fn edit_preserves_lifecycle_meta() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        eng.remember("k", "original content", vec![]).unwrap();
        eng.recall("original", 5).unwrap(); // verified
        assert!(eng.edit("k", "new content here").unwrap());
        let items = eng.list_with_lifecycle(MemoryCategory::Task).unwrap();
        let (e, meta) = items.iter().find(|(e, _)| e.id == "k").unwrap();
        assert_eq!(e.content, "new content here");
        assert_eq!(
            meta.stage.as_str(),
            "verified",
            "edit 不得重置 lifecycle stage"
        );
        assert_eq!(meta.recall_count, 1, "edit 不得清零 recall_count");
        // 缺失 id → false。
        assert!(!eng.edit("nope", "x").unwrap());
    }

    #[test]
    fn replay_hybrid_breaks_down_scores() {
        let eng = MemoryEngine::open_in_memory_with_embedder(
            true,
            Some(Arc::new(FakeEmbed)),
            Some("test-model".to_string()),
        )
        .unwrap();
        eng.remember("k", "ferris crab language", vec![]).unwrap();
        let hits = eng.replay("rust", 5, 0.3).unwrap();
        assert_eq!(hits.len(), 1, "语义独有命中必须被召回");
        let h = &hits[0];
        assert!(h.hybrid, "有 embedder 应走混合检索");
        assert_eq!(h.entry.id, "k");
        assert_eq!(h.bm25, 0.0, "无 FTS 共词时 bm25 分量应为 0");
        assert!(h.cosine > 0.0, "语义独有命中应贡献余弦分: {h:?}");
        assert!(h.lifecycle <= 0.0, "生命周期惩罚为负数");
        assert!(
            (h.score - (h.bm25 + h.cosine + h.lifecycle)).abs() < 1e-9,
            "分解必须自洽: {h:?}"
        );
    }

    #[test]
    fn replay_fts_path_has_zero_cosine_and_no_state_mutation() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        eng.remember("k", "rust borrow checker", vec![]).unwrap();
        let hits = eng.replay("rust", 5, 0.3).unwrap();
        assert_eq!(hits.len(), 1);
        let h = &hits[0];
        assert!(!h.hybrid, "无 embedder 应走纯 FTS");
        assert_eq!(h.cosine, 0.0);
        assert!(h.bm25 > 0.0);
        assert!(
            (h.score - (h.bm25 + h.lifecycle)).abs() < 1e-9,
            "FTS 分解必须自洽: {h:?}"
        );
        // 回放是只读诊断：不得记召回、不得晋级 lifecycle。
        let (e, meta) = &eng.list_with_lifecycle(MemoryCategory::Task).unwrap()[0];
        assert_eq!(meta.stage.as_str(), "candidate", "replay 不得晋级 stage");
        assert_eq!(meta.recall_count, 0, "replay 不得记 recall_count");
        assert_eq!(e.content, "rust borrow checker");
        let (calls, nonempty) = eng.store.recall_counters().unwrap();
        assert_eq!((calls, nonempty), (0, 0), "replay 不得污染召回命中率统计");
    }
}
