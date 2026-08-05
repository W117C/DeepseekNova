//! # Auto-Skill System — Extract reusable skills from task experience
//!
//! Inspired by Hermes Agent's closed learning loop:
//! 1. Execute task
//! 2. Evaluate result
//! 3. Extract skill (if task was complex enough)
//! 4. Store to skill library
//! 5. Refine on future use
//!
//! Skills are stored as Markdown + YAML frontmatter, compatible with agentskills.io.
//!
//! 热更新（设计 C）：蒸馏自动生成的 skill 写入 `<skill_dir>/auto/` 子目录，
//! 与用户手写 skill 隔离；frontmatter 注入 `source: distill` / `state: draft`
//! 等元数据键（`SkillFrontmatter` 结构本身不含这些字段，旧文件解析回退为
//! 用户手写来源，天然豁免清理）。`reload()` 可在会话边界重新扫描目录。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Frontmatter metadata for a skill file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillFrontmatter {
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub use_count: u32,
    #[serde(default)]
    pub success_count: u32,
    #[serde(default)]
    pub source_session: Option<String>,
}

/// A complete skill file (frontmatter + body).
#[derive(Debug, Clone)]
pub struct Skill {
    pub frontmatter: SkillFrontmatter,
    pub body: String,
}

impl Skill {
    /// Serialize to Markdown with YAML frontmatter.
    pub fn to_markdown(&self) -> String {
        let yaml = serde_yaml::to_string(&self.frontmatter).unwrap_or_default();
        format!("---\n{yaml}---\n\n{}\n", self.body)
    }

    /// Parse from Markdown with YAML frontmatter.
    pub fn from_markdown(content: &str) -> Option<Self> {
        let content = content.trim();
        if !content.starts_with("---") {
            return None;
        }
        let end = content[3..].find("---")?;
        let yaml_part = &content[3..3 + end];
        let body = content[3 + end + 3..].trim().to_string();

        let frontmatter: SkillFrontmatter = serde_yaml::from_str(yaml_part).ok()?;
        Some(Self { frontmatter, body })
    }
}

/// Skill extraction input — what the agent observed during task execution.
#[derive(Debug, Clone)]
pub struct TaskObservation {
    pub task_description: String,
    pub tool_calls: Vec<String>,
    pub steps_taken: Vec<String>,
    pub outcome: TaskOutcome,
    pub user_feedback: Option<String>,
    pub session_id: String,
    /// 任务触碰的文件路径（写类工具参数提取），用于任务-文件关联沉淀（P3.3）。
    pub files: Vec<String>,
}

/// Outcome of a task execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskOutcome {
    Success,
    PartialSuccess,
    Failure,
}

/// Configuration for skill auto-extraction.
#[derive(Debug, Clone)]
pub struct SkillExtractionConfig {
    /// Minimum tool calls to trigger skill extraction.
    pub min_tool_calls: usize,
    /// Minimum steps to trigger skill extraction.
    pub min_steps: usize,
    /// Skill library directory.
    pub skill_dir: PathBuf,
    /// `draft` → `verified` 的 `use_count` 阈值（默认对齐
    /// [`VERIFY_USE_THRESHOLD`]，可经 `[memory] verify_use_threshold` 注入）。
    pub verify_use_threshold: u32,
    /// `verified` → `active` 的跨会话出现次数阈值（默认对齐
    /// [`ACTIVE_SESSION_THRESHOLD`]，可经 `[memory] active_session_threshold` 注入）。
    pub active_session_threshold: u32,
    /// 自动保留的 distill draft 数量上限（默认对齐 [`MAX_AUTO_DRAFT_SKILLS`]，
    /// 可经 `[memory] max_auto_draft_skills` 注入；超出按 LRU 清理）。
    pub max_auto_draft_skills: usize,
}

impl Default for SkillExtractionConfig {
    fn default() -> Self {
        Self {
            min_tool_calls: 5,
            min_steps: 3,
            skill_dir: PathBuf::from(".deepseeknova/skills"),
            verify_use_threshold: VERIFY_USE_THRESHOLD,
            active_session_threshold: ACTIVE_SESSION_THRESHOLD,
            max_auto_draft_skills: MAX_AUTO_DRAFT_SKILLS,
        }
    }
}

/// Skill 来源：蒸馏自动生成 vs 用户手写装载。
///
/// 序列化为 frontmatter 的 `source` 键（`distill` / `user`）。用户手写
/// skill 文件无该键，解析回退为 `User`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SkillSource {
    /// 用户手写/装载的 skill——清理完全豁免。
    #[default]
    User,
    /// LLM 蒸馏自动生成的 skill——仅 `Draft` 态可被自动清理。
    Distill,
}

/// 蒸馏 skill 的质量三态（注入强度递进）。
///
/// 序列化为 frontmatter 的 `state` 键（`draft` / `verified` / `active`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SkillState {
    /// 落盘初始态：低优先级试用注入（仅高匹配度时），`record_use` 统计效果。
    #[default]
    Draft,
    /// `record_use` 达标（见 [`VERIFY_USE_THRESHOLD`]）后转正：常规 recall 注入。
    Verified,
    /// 跨会话存活达标（见 [`ACTIVE_SESSION_THRESHOLD`]）后：长期保留，清理豁免。
    Active,
}

/// `draft` → `verified` 的 `use_count` 阈值。
pub const VERIFY_USE_THRESHOLD: u32 = 3;

/// `verified` → `active` 的跨会话出现次数阈值。
pub const ACTIVE_SESSION_THRESHOLD: u32 = 3;

/// 自动保留的 distill draft 数量上限（超出部分按 LRU 清理）。
pub const MAX_AUTO_DRAFT_SKILLS: usize = 20;

/// 蒸馏 skill 的运行时元数据。持久化在 skill 文件 frontmatter 的
/// `source` / `state` / `sessions_seen` / `last_session` 键中，`reload()`
/// 后从文件恢复、状态不丢。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SkillMeta {
    source: SkillSource,
    state: SkillState,
    /// 出现过的不同会话数（跨会话存活度）。以 `sessions` 集合去重计数；
    /// 直接持久化 `sessions_seen` 保持旧文件兼容（Bugbot 审查 MEDIUM 修复：
    /// 原实现按“会话切换次数”计数，A/B/A/B 会虚增为 4）。
    sessions_seen: u32,
    /// 已出现过的会话 id 集合（去重，跨 reload 保留）。
    #[serde(default)]
    sessions: Vec<String>,
    /// 最近一次使用的会话 id。
    last_session: Option<String>,
}

impl Default for SkillMeta {
    fn default() -> Self {
        Self {
            source: SkillSource::User,
            state: SkillState::Draft,
            sessions_seen: 0,
            sessions: Vec::new(),
            last_session: None,
        }
    }
}

/// 内存中的 skill 条目：skill 内容 + 磁盘路径 + 运行时元数据。
struct ManagedSkill {
    skill: Skill,
    path: PathBuf,
    meta: SkillMeta,
}

/// 提取 frontmatter 的 YAML 段（不含首尾 `---`）。
fn frontmatter_yaml(content: &str) -> Option<&str> {
    let content = content.trim();
    if !content.starts_with("---") {
        return None;
    }
    let end = content[3..].find("---")?;
    Some(&content[3..3 + end])
}

/// 从 Markdown 文本恢复元数据；缺键/无法解析一律回退默认（用户手写豁免）。
fn meta_from_markdown(content: &str) -> SkillMeta {
    let mut meta = SkillMeta::default();
    let Some(yaml_part) = frontmatter_yaml(content) else {
        return meta;
    };
    let Ok(v) = serde_yaml::from_str::<serde_yaml::Value>(yaml_part) else {
        return meta;
    };
    let Some(map) = v.as_mapping() else {
        return meta;
    };
    if let Some(s) = map.get("source") {
        if let Ok(src) = serde_yaml::from_value::<SkillSource>(s.clone()) {
            meta.source = src;
        }
    }
    if let Some(s) = map.get("state") {
        if let Ok(st) = serde_yaml::from_value::<SkillState>(s.clone()) {
            meta.state = st;
        }
    }
    if let Some(s) = map.get("sessions_seen") {
        if let Some(n) = s.as_u64() {
            meta.sessions_seen = n.min(u32::MAX as u64) as u32;
        }
    }
    if let Some(s) = map.get("sessions") {
        if let Some(v) = s.as_sequence() {
            meta.sessions = v
                .iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect();
        }
    }
    if let Some(s) = map.get("last_session") {
        meta.last_session = s.as_str().map(str::to_string);
    }
    meta
}

/// 序列化 skill 文件：frontmatter 基础字段 + 注入元数据键（`source` 等）。
/// `SkillFrontmatter` 结构不含这些键，`Skill::from_markdown` 解析时忽略未知键，
/// 故用户手写文件与旧文件格式不受影响。
fn skill_markdown_with_meta(skill: &Skill, meta: &SkillMeta) -> String {
    let mut map: serde_yaml::Mapping =
        serde_yaml::from_str(&serde_yaml::to_string(&skill.frontmatter).unwrap_or_default())
            .unwrap_or_default();
    if let serde_yaml::Value::Mapping(m) = serde_yaml::to_value(meta).unwrap_or_default() {
        for (k, v) in m {
            map.insert(k, v);
        }
    }
    let yaml = serde_yaml::to_string(&serde_yaml::Value::Mapping(map)).unwrap_or_default();
    format!("---\n{yaml}---\n\n{}\n", skill.body)
}

/// The skill manager handles extraction, storage, and retrieval.
pub struct SkillManager {
    config: SkillExtractionConfig,
    /// In-memory cache of loaded skills (content + path + runtime meta).
    skills: HashMap<String, ManagedSkill>,
}

impl SkillManager {
    pub fn new(config: SkillExtractionConfig) -> Self {
        let mut manager = Self {
            config,
            skills: HashMap::new(),
        };
        manager.load_skills().ok();
        manager
    }

    /// Load all skills from the skill directory (recursively, including
    /// the `auto/` subdirectory for distilled skills).
    fn load_skills(&mut self) -> anyhow::Result<()> {
        let dir = self.config.skill_dir.clone();
        if !dir.exists() {
            return Ok(());
        }
        self.load_dir(&dir)?;
        info!(count = self.skills.len(), "loaded skills");
        Ok(())
    }

    fn load_dir(&mut self, dir: &Path) -> anyhow::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                self.load_dir(&path)?;
                continue;
            }
            if path.extension().map(|e| e == "md").unwrap_or(false) {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Some(skill) = Skill::from_markdown(&content) {
                        let name = skill.frontmatter.name.clone();
                        let meta = meta_from_markdown(&content);
                        self.skills.insert(name, ManagedSkill { skill, path, meta });
                    }
                }
            }
        }
        Ok(())
    }

    /// Re-scan the skill directory (including `auto/`) and rebuild the
    /// in-memory cache. Runtime state (source/state/sessions) is persisted in
    /// the skill file frontmatter, so it survives reloads.
    pub fn reload(&mut self) -> anyhow::Result<()> {
        self.skills.clear();
        self.load_skills()
    }

    /// Evaluate whether a task observation warrants skill extraction.
    pub fn should_extract_skill(&self, obs: &TaskObservation) -> bool {
        obs.tool_calls.len() >= self.config.min_tool_calls
            && obs.steps_taken.len() >= self.config.min_steps
            && obs.outcome != TaskOutcome::Failure
    }

    /// Create a skill from a task observation.
    /// The actual content extraction is done by the LLM — this handles storage.
    /// User-authored skills are written to the skill directory root (no
    /// `source`/`state` meta keys → always exempt from auto-cleanup).
    pub fn create_skill(&mut self, skill: Skill) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.config.skill_dir)?;
        let filename = format!(
            "{}/{}.md",
            self.config.skill_dir.display(),
            skill.frontmatter.name.replace(' ', "-").to_lowercase()
        );
        std::fs::write(&filename, skill.to_markdown())?;
        info!(name = %skill.frontmatter.name, path = %filename, "skill created");
        let name = skill.frontmatter.name.clone();
        self.skills.insert(
            name,
            ManagedSkill {
                skill,
                path: PathBuf::from(&filename),
                meta: SkillMeta::default(),
            },
        );
        Ok(())
    }

    /// Create a distilled auto-generated skill: forced `source: distill` +
    /// `state: draft`, written to `<skill_dir>/auto/` (isolated from
    /// user-authored skills). This is the entry point for the distill →
    /// skill file → recall loop (design C).
    pub fn create_distilled_skill(
        &mut self,
        title: &str,
        body: &str,
        tags: Vec<String>,
        source_session: Option<&str>,
    ) -> anyhow::Result<()> {
        let title = title.trim();
        if title.is_empty() {
            anyhow::bail!("distilled skill title is empty");
        }
        let now = chrono::Utc::now().to_rfc3339();
        // slug 白名单化：LLM 蒸馏产出的 title 不可信，含路径分隔符/.. 的
        // title 直接拒绝，防目录逃逸写入（审查 HIGH-1）。其余字符中保留
        // Unicode 字母数字（含 CJK，中文标题蒸馏产物很常见）与 `-`，其余
        // 映射为 `-`；纯标点/空白标题仍会产出空 slug 并拒绝。
        if title.contains('/') || title.contains('\\') || title.contains("..") {
            anyhow::bail!("distilled skill title contains path separators: {title:?}");
        }
        let name: String = title
            .to_lowercase()
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .trim_matches('-')
            .to_string();
        if name.is_empty() {
            anyhow::bail!("distilled skill title produced an empty slug: {title:?}");
        }
        // 标点类标题可能缩成 "." / ".."（如 `。.`），会生成隐藏/危险文件名。
        if name == "." || name == ".." {
            anyhow::bail!("distilled skill title produced a dot-only slug: {title:?}");
        }
        let skill = Skill {
            frontmatter: SkillFrontmatter {
                name: name.clone(),
                version: "0.1.0".into(),
                description: title.to_string(),
                triggers: vec![],
                tags,
                created_at: now.clone(),
                updated_at: now,
                use_count: 0,
                success_count: 0,
                source_session: source_session.map(str::to_string),
            },
            body: body.to_string(),
        };
        // 同名防护：蒸馏产物不得静默覆盖用户手写 skill 的内存条目
        // （磁盘文件不动，但 reload 后 read_dir 顺序不确定，胜者不稳定——
        // Bugbot 审查 MEDIUM 修复）。同名蒸馏重写（source=Distill）是
        // 期望行为，允许覆盖。检查先于写盘，避免失败时留下孤儿文件。
        if let Some(existing) = self.skills.get(&name) {
            if existing.meta.source != SkillSource::Distill {
                anyhow::bail!(
                    "distilled skill name '{name}' collides with user skill (source={:?})",
                    existing.meta.source
                );
            }
        }
        let auto_dir = self.config.skill_dir.join("auto");
        std::fs::create_dir_all(&auto_dir)?;
        let path = auto_dir.join(format!("{name}.md"));
        let meta = SkillMeta {
            source: SkillSource::Distill,
            state: SkillState::Draft,
            sessions_seen: 0,
            sessions: Vec::new(),
            last_session: None,
        };
        std::fs::write(&path, skill_markdown_with_meta(&skill, &meta))?;
        info!(name = %name, path = %path.display(), "distilled skill created (draft)");
        self.skills.insert(name, ManagedSkill { skill, path, meta });
        Ok(())
    }

    /// Record a skill use and persist updated statistics.
    ///
    /// For distilled skills this drives the quality state machine:
    /// - `use_count` reaches the configured verify threshold
    ///   (default [`VERIFY_USE_THRESHOLD`]) → `draft` → `verified`
    /// - distinct sessions reach the configured active threshold
    ///   (default [`ACTIVE_SESSION_THRESHOLD`]) → `verified` → `active`
    ///
    /// Returns the skill's state after the update (`None` if not found).
    pub fn record_use(
        &mut self,
        skill_name: &str,
        success: bool,
        session_id: Option<&str>,
    ) -> anyhow::Result<Option<SkillState>> {
        let Some(ms) = self.skills.get_mut(skill_name) else {
            return Ok(None);
        };
        ms.skill.frontmatter.use_count += 1;
        if success {
            ms.skill.frontmatter.success_count += 1;
        }
        ms.skill.frontmatter.updated_at = chrono::Utc::now().to_rfc3339();
        if ms.meta.source == SkillSource::Distill {
            if let Some(sid) = session_id {
                if !ms.meta.sessions.iter().any(|s| s == sid) {
                    ms.meta.sessions.push(sid.to_string());
                    ms.meta.sessions_seen = ms.meta.sessions.len() as u32;
                    ms.meta.last_session = Some(sid.to_string());
                }
            }
            if ms.meta.state == SkillState::Draft
                && ms.skill.frontmatter.use_count >= self.config.verify_use_threshold
            {
                ms.meta.state = SkillState::Verified;
            }
            if ms.meta.state == SkillState::Verified
                && ms.meta.sessions_seen >= self.config.active_session_threshold
            {
                ms.meta.state = SkillState::Active;
            }
        }
        let content = if ms.meta.source == SkillSource::Distill {
            skill_markdown_with_meta(&ms.skill, &ms.meta)
        } else {
            ms.skill.to_markdown()
        };
        std::fs::write(&ms.path, content)?;
        Ok(Some(ms.meta.state))
    }

    /// Query a skill's current state (`None` if not found).
    pub fn skill_state(&self, name: &str) -> Option<SkillState> {
        self.skills.get(name).map(|ms| ms.meta.state)
    }

    /// Auto-cleanup (LRU): remove distilled `draft` skills beyond `max_retain`,
    /// ordered by `use_count` asc then `updated_at` asc. User-authored,
    /// `verified`, and `active` skills are always exempt — never deleted.
    /// Returns the number of removed skills.
    pub fn prune_auto_drafts(&mut self, max_retain: usize) -> anyhow::Result<usize> {
        let mut candidates: Vec<(String, PathBuf, u32, String)> = self
            .skills
            .iter()
            .filter(|(_, ms)| {
                ms.meta.source == SkillSource::Distill && ms.meta.state == SkillState::Draft
            })
            .map(|(name, ms)| {
                (
                    name.clone(),
                    ms.path.clone(),
                    ms.skill.frontmatter.use_count,
                    ms.skill.frontmatter.updated_at.clone(),
                )
            })
            .collect();
        if candidates.len() <= max_retain {
            return Ok(0);
        }
        candidates.sort_by(|a, b| {
            a.2.cmp(&b.2)
                .then_with(|| a.3.cmp(&b.3))
                .then_with(|| a.0.cmp(&b.0))
        });
        let mut removed = 0;
        let retain = candidates.len() - max_retain;
        for (name, path, _, _) in candidates.into_iter().take(retain) {
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    self.skills.remove(&name);
                    removed += 1;
                }
                Err(e) => warn!(path = %path.display(), "prune remove failed: {e}"),
            }
        }
        info!(removed, "pruned auto draft skills");
        Ok(removed)
    }

    /// Find skills matching a query (by name, tags, triggers, or body).
    ///
    /// 注入强度分层（设计 C）：distilled `draft` skills are trial-injected at
    /// low priority — they must match name/triggers/tags (no body fuzzy match)
    /// and rank last, so consumers capping results (e.g. recall `max_skills`)
    /// naturally see them only when few higher-priority skills match.
    /// `active`/user-authored rank first, `verified` second, distilled `draft` last.
    pub fn find_matching_skills(&self, query: &str) -> Vec<&Skill> {
        let query_lower = query.to_lowercase();
        let mut matched: Vec<(&Skill, u8, usize)> = Vec::new();
        for (name, ms) in &self.skills {
            let s = &ms.skill;
            let strong = name.to_lowercase().contains(&query_lower)
                || s.frontmatter
                    .triggers
                    .iter()
                    .any(|t| t.to_lowercase().contains(&query_lower))
                || s.frontmatter
                    .tags
                    .iter()
                    .any(|t| t.to_lowercase().contains(&query_lower));
            let weak = s.body.to_lowercase().contains(&query_lower);
            let is_distill_draft =
                ms.meta.source == SkillSource::Distill && ms.meta.state == SkillState::Draft;
            if is_distill_draft {
                if !strong {
                    continue;
                }
            } else if !strong && !weak {
                continue;
            }
            let rank = match ms.meta.source {
                SkillSource::Distill => match ms.meta.state {
                    SkillState::Active => 0,
                    SkillState::Verified => 1,
                    SkillState::Draft => 2,
                },
                SkillSource::User => 0,
            };
            matched.push((s, rank, matched.len()));
        }
        matched.sort_by_key(|(_, rank, seq)| (*rank, *seq));
        matched.into_iter().map(|(s, _, _)| s).collect()
    }

    /// Get all skills.
    pub fn list_skills(&self) -> Vec<&Skill> {
        self.skills.values().map(|ms| &ms.skill).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skill_markdown_roundtrip() {
        let skill = Skill {
            frontmatter: SkillFrontmatter {
                name: "test-skill".into(),
                version: "1.0.0".into(),
                description: "A test skill".into(),
                triggers: vec!["test".into()],
                tags: vec!["testing".into()],
                created_at: "2026-01-01T00:00:00Z".into(),
                updated_at: "2026-01-01T00:00:00Z".into(),
                use_count: 0,
                success_count: 0,
                source_session: None,
            },
            body: "# Test Skill\n\nThis is a test.".into(),
        };

        let markdown = skill.to_markdown();
        let parsed = Skill::from_markdown(&markdown).expect("should parse");

        assert_eq!(parsed.frontmatter.name, skill.frontmatter.name);
        assert_eq!(
            parsed.frontmatter.description,
            skill.frontmatter.description
        );
        assert!(parsed.body.contains("Test Skill"));
    }

    #[test]
    fn test_should_extract() {
        let manager = SkillManager::new(SkillExtractionConfig::default());
        let obs = TaskObservation {
            task_description: "Build a web server".into(),
            tool_calls: vec!["write_file".into(); 6],
            steps_taken: vec!["step1".into(), "step2".into(), "step3".into()],
            outcome: TaskOutcome::Success,
            user_feedback: None,
            session_id: "s1".into(),
            files: vec![],
        };
        assert!(manager.should_extract_skill(&obs));

        let obs_small = TaskObservation {
            task_description: "Quick question".into(),
            tool_calls: vec!["read_file".into()],
            steps_taken: vec!["read".into()],
            outcome: TaskOutcome::Success,
            user_feedback: None,
            session_id: "s1".into(),
            files: vec![],
        };
        assert!(!manager.should_extract_skill(&obs_small));

        let obs_fail = TaskObservation {
            task_description: "Failed task".into(),
            tool_calls: vec!["write_file".into(); 10],
            steps_taken: vec!["step1".into(); 5],
            outcome: TaskOutcome::Failure,
            user_feedback: None,
            session_id: "s1".into(),
            files: vec![],
        };
        assert!(!manager.should_extract_skill(&obs_fail));
    }

    #[test]
    fn test_skill_match() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let config = SkillExtractionConfig {
            skill_dir: temp.path().parent().unwrap().join(".test-skills"),
            ..Default::default()
        };
        let mut manager = SkillManager::new(config);

        manager
            .create_skill(Skill {
                frontmatter: SkillFrontmatter {
                    name: "rust-testing".into(),
                    version: "1.0.0".into(),
                    description: "How to write Rust tests".into(),
                    triggers: vec!["write tests".into(), "unit tests".into()],
                    tags: vec!["rust".into(), "testing".into()],
                    created_at: "2026-01-01T00:00:00Z".into(),
                    updated_at: "2026-01-01T00:00:00Z".into(),
                    use_count: 0,
                    success_count: 0,
                    source_session: None,
                },
                body: "Use #[test] attribute and assert_eq!".into(),
            })
            .unwrap();

        let matches = manager.find_matching_skills("rust");
        assert_eq!(matches.len(), 1);

        let matches = manager.find_matching_skills("write tests");
        assert_eq!(matches.len(), 1);

        let matches = manager.find_matching_skills("python");
        assert!(matches.is_empty());
    }

    /// 构造临时 SkillManager（skill_dir 指向 tempdir）。
    fn manager_in(dir: &Path) -> SkillManager {
        SkillManager::new(SkillExtractionConfig {
            skill_dir: dir.to_path_buf(),
            ..Default::default()
        })
    }

    #[test]
    fn test_distilled_skill_state_machine() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = manager_in(dir.path());
        m.create_distilled_skill("Fix Auth Flow", "body", vec!["auth".into()], Some("s1"))
            .unwrap();
        assert_eq!(m.skill_state("fix-auth-flow"), Some(SkillState::Draft));

        // 同会话使用 3 次 → use_count 达标 → verified
        assert_eq!(
            m.record_use("fix-auth-flow", true, Some("s1")).unwrap(),
            Some(SkillState::Draft)
        );
        assert_eq!(
            m.record_use("fix-auth-flow", true, Some("s1")).unwrap(),
            Some(SkillState::Draft)
        );
        assert_eq!(
            m.record_use("fix-auth-flow", true, Some("s1")).unwrap(),
            Some(SkillState::Verified)
        );

        // 再跨 2 个新会话出现 → sessions_seen 达 3 → active
        assert_eq!(
            m.record_use("fix-auth-flow", true, Some("s2")).unwrap(),
            Some(SkillState::Verified)
        );
        assert_eq!(
            m.record_use("fix-auth-flow", true, Some("s3")).unwrap(),
            Some(SkillState::Active)
        );
        // active 后保持
        assert_eq!(
            m.record_use("fix-auth-flow", true, Some("s4")).unwrap(),
            Some(SkillState::Active)
        );
        // 状态迁移只发生在 distill skill 上；用户手写 skill 不受影响
        m.create_skill(Skill {
            frontmatter: SkillFrontmatter {
                name: "handwritten".into(),
                version: "1.0.0".into(),
                description: "d".into(),
                triggers: vec![],
                tags: vec![],
                created_at: "2026-01-01T00:00:00Z".into(),
                updated_at: "2026-01-01T00:00:00Z".into(),
                use_count: 0,
                success_count: 0,
                source_session: None,
            },
            body: "b".into(),
        })
        .unwrap();
        assert_eq!(
            m.record_use("handwritten", true, Some("s9")).unwrap(),
            Some(SkillState::Draft)
        );
        assert_eq!(m.skill_state("handwritten"), Some(SkillState::Draft));
        // 不存在的 skill → None
        assert_eq!(m.record_use("nope", true, Some("s1")).unwrap(), None);
    }

    #[test]
    fn test_prune_only_removes_distill_drafts() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = manager_in(dir.path());
        // 用户手写 skill（根目录，无 source 键）
        m.create_skill(Skill {
            frontmatter: SkillFrontmatter {
                name: "user-skill".into(),
                version: "1.0.0".into(),
                description: "user authored".into(),
                triggers: vec!["user trigger".into()],
                tags: vec!["user".into()],
                created_at: "2026-01-01T00:00:00Z".into(),
                updated_at: "2026-01-01T00:00:00Z".into(),
                use_count: 0,
                success_count: 0,
                source_session: None,
            },
            body: "user body".into(),
        })
        .unwrap();
        // 两个 distill draft
        m.create_distilled_skill("Draft Alpha", "a", vec![], Some("s1"))
            .unwrap();
        m.create_distilled_skill("Draft Beta", "b", vec![], Some("s1"))
            .unwrap();
        // 一个 distill verified（豁免）
        m.create_distilled_skill("Promoted", "p", vec![], Some("s1"))
            .unwrap();
        for _ in 0..VERIFY_USE_THRESHOLD {
            m.record_use("promoted", true, Some("s1")).unwrap();
        }
        assert_eq!(m.skill_state("promoted"), Some(SkillState::Verified));

        // max_retain=1：2 个 draft → 删 1 个（LRU：use_count 相同 → updated_at 旧者先删）
        let removed = m.prune_auto_drafts(1).unwrap();
        assert_eq!(removed, 1);
        // 只剩 1 个 draft；verified 与用户手写均保留
        let draft_names: Vec<String> = m
            .list_skills()
            .iter()
            .map(|s| s.frontmatter.name.clone())
            .filter(|n| n.starts_with("draft-"))
            .collect();
        assert_eq!(draft_names.len(), 1);
        assert_eq!(m.skill_state("promoted"), Some(SkillState::Verified));
        assert!(m.skill_state("user-skill").is_some());
        // 磁盘：auto/ 下只剩 1 个文件，根目录手写文件仍在
        let auto_files = std::fs::read_dir(dir.path().join("auto")).unwrap().count();
        assert_eq!(auto_files, 2); // 1 draft + 1 promoted
        assert!(dir.path().join("user-skill.md").exists());
        // 再次清理（max_retain=0）→ 删掉最后 1 个 draft，但 promoted/user 仍豁免
        assert_eq!(m.prune_auto_drafts(0).unwrap(), 1);
        assert_eq!(
            m.skill_state("draft-alpha")
                .or_else(|| m.skill_state("draft-beta")),
            None
        );
        assert_eq!(m.skill_state("promoted"), Some(SkillState::Verified));
        assert_eq!(m.skill_state("user-skill"), Some(SkillState::Draft));
        assert!(dir.path().join("user-skill.md").exists());
    }

    #[test]
    fn test_reload_preserves_state() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = manager_in(dir.path());
        m.create_distilled_skill("Keep Draft", "d", vec![], Some("s1"))
            .unwrap();
        m.create_distilled_skill("Keep Verified", "v", vec![], Some("s1"))
            .unwrap();
        for _ in 0..VERIFY_USE_THRESHOLD {
            m.record_use("keep-verified", true, Some("s1")).unwrap();
        }
        assert_eq!(m.skill_state("keep-verified"), Some(SkillState::Verified));

        // reload 后从 frontmatter 恢复状态
        m.reload().unwrap();
        assert_eq!(m.skill_state("keep-draft"), Some(SkillState::Draft));
        assert_eq!(m.skill_state("keep-verified"), Some(SkillState::Verified));

        // 全新实例（模拟下一会话）同样恢复
        let m2 = manager_in(dir.path());
        assert_eq!(m2.skill_state("keep-draft"), Some(SkillState::Draft));
        assert_eq!(m2.skill_state("keep-verified"), Some(SkillState::Verified));
    }

    #[test]
    fn test_distilled_skill_written_to_auto_dir_with_source_marker() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = manager_in(dir.path());
        m.create_distilled_skill(
            "Use Serde Derive",
            "Prefer derive",
            vec!["serde".into()],
            Some("s1"),
        )
        .unwrap();
        let path = dir.path().join("auto/use-serde-derive.md");
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("source: distill"),
            "frontmatter 必须含 source: distill"
        );
        assert!(
            content.contains("state: draft"),
            "frontmatter 必须含 state: draft"
        );
        // 用户手写 skill 文件不含这些键
        m.create_skill(Skill {
            frontmatter: SkillFrontmatter {
                name: "manual".into(),
                version: "1.0.0".into(),
                description: "d".into(),
                triggers: vec![],
                tags: vec![],
                created_at: "2026-01-01T00:00:00Z".into(),
                updated_at: "2026-01-01T00:00:00Z".into(),
                use_count: 0,
                success_count: 0,
                source_session: None,
            },
            body: "b".into(),
        })
        .unwrap();
        assert!(!std::fs::read_to_string(dir.path().join("manual.md"))
            .unwrap()
            .contains("source:"));
    }

    #[test]
    fn test_distilled_skill_accepts_cjk_titles() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = manager_in(dir.path());
        // 中文标题必须能落盘（Unicode 字母数字保留，不能整体变成空 slug）。
        m.create_distilled_skill("修复登录流程", "校验 token 后再路由", vec![], Some("s1"))
            .unwrap();
        let path = dir.path().join("auto/修复登录流程.md");
        assert!(
            path.exists(),
            "CJK distilled skill must be written to auto/"
        );
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("source: distill"));
        assert_eq!(m.skill_state("修复登录流程"), Some(SkillState::Draft));
        // 混合中英文标题：非字母数字（空格等）映射为 '-'。
        m.create_distilled_skill("Fix Auth 登录", "b", vec![], Some("s1"))
            .unwrap();
        assert!(dir.path().join("auto/fix-auth-登录.md").exists());
        // 纯标点标题仍拒绝（空 slug）。
        assert!(m
            .create_distilled_skill("!!!", "b", vec![], Some("s1"))
            .is_err());
        // 点状标题缩成 "." / ".." 同样拒绝（防隐藏/危险文件名）。
        assert!(m
            .create_distilled_skill("。.", "b", vec![], Some("s1"))
            .is_err());
    }

    #[test]
    fn test_draft_ranks_last_and_requires_strong_match() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = manager_in(dir.path());
        // draft：仅 body 含查询 → 不注入（弱匹配被挡）
        m.create_distilled_skill(
            "Trial One",
            "unique phrase in body only",
            vec![],
            Some("s1"),
        )
        .unwrap();
        // draft：tag 命中 → 注入但排最后
        m.create_distilled_skill("Trial Two", "b", vec!["sharedtag".into()], Some("s1"))
            .unwrap();
        // verified：body 命中 → 常规注入
        m.create_distilled_skill("Proven", "sharedtag body text", vec![], Some("s1"))
            .unwrap();
        for _ in 0..VERIFY_USE_THRESHOLD {
            m.record_use("proven", true, Some("s1")).unwrap();
        }

        let by_body = m.find_matching_skills("unique phrase");
        assert!(by_body.is_empty(), "draft 弱匹配（仅 body）不得注入");

        let by_tag = m.find_matching_skills("sharedtag");
        assert_eq!(by_tag.len(), 2, "verified + draft 都应命中");
        assert_eq!(
            by_tag[0].frontmatter.name, "proven",
            "verified 排在 draft 之前"
        );
        assert_eq!(
            by_tag[1].frontmatter.name, "trial-two",
            "draft 排最后（低优先级）"
        );
    }
}
