//! Skill system for deepseeknova.
//!
//! Skills are reusable prompt templates stored as markdown files with YAML
//! frontmatter in `.deepseeknova/skills/`. Each skill is exposed as a tool so
//! the agent can activate it during a conversation.
//!
//! ## Quick start
//!
//! ```no_run
//! use deepseeknova_skills::{SkillLoader, SkillTool};
//! use std::sync::Arc;
//!
//! // Load skills from the project's .deepseeknova/skills/ directory
//! let loader = SkillLoader::new(".deepseeknova/skills");
//! let skills = loader.load_all().unwrap();
//!
//! // Wrap each skill as a Tool for the registry
//! let tools: Vec<Arc<dyn deepseeknova_core::Tool>> = skills
//!     .into_iter()
//!     .map(|s| Arc::new(SkillTool::new(s)) as Arc<dyn deepseeknova_core::Tool>)
//!     .collect();
//! ```

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        clippy::dbg_macro
    )
)]

mod loader;

pub mod fitness;

use std::path::PathBuf;

pub use loader::SkillLoader;

/// Path to the built-in skills bundled with this crate.
pub const BUILTIN_SKILLS_DIR: &str = "builtin";

/// 内置技能源文件列表（`(文件名, 编译期内嵌内容)`）。
///
/// 用 `include_str!` 把 `builtin/*.md` 内嵌进二进制，运行时不再依赖编译期
/// `CARGO_MANIFEST_DIR` 磁盘路径——`cargo install` / 分发 release 二进制到
/// 其他机器后技能依然存在。`BUILTIN_SKILLS_DIR` 常量保留作文档与 /skills
/// 的兼容引用。
const BUILTIN_SKILL_FILES: &[(&str, &str)] = &[
    (
        "adversarial-review",
        include_str!("../builtin/adversarial-review.md"),
    ),
    (
        "coding-copilot",
        include_str!("../builtin/coding-copilot.md"),
    ),
    ("dna-spec", include_str!("../builtin/dna-spec.md")),
    (
        "first-principles",
        include_str!("../builtin/first-principles.md"),
    ),
    (
        "frontend-developer",
        include_str!("../builtin/frontend-developer.md"),
    ),
    (
        "loop-engineering",
        include_str!("../builtin/loop-engineering.md"),
    ),
];

/// Load all built-in skills shipped with the deepseeknova-skills crate.
///
/// These are the default cognitive frameworks that every DeepseekNova
/// agent starts with:
/// - `frontend-developer` — UI/UX design and code generation
/// - `coding-copilot` — multi-language coding assistant
/// - `loop-engineering` — iterative improvement loop
/// - `first-principles` — first-principles reasoning
/// - `adversarial-review` — hostile red-team review
///
/// 内容编译期内嵌（见 `BUILTIN_SKILL_FILES`），单条解析失败只 warn 不整体失败。
pub fn load_builtin_skills() -> Vec<Skill> {
    let mut skills = Vec::new();
    for (name, raw) in BUILTIN_SKILL_FILES {
        match loader::parse_skill_str(raw, SkillScope::Builtin) {
            Ok(skill) => {
                debug_assert_eq!(
                    skill.name, *name,
                    "内置技能文件名应与 frontmatter name 一致"
                );
                skills.push(skill);
            }
            Err(e) => {
                tracing::warn!(name = *name, error = %e, "failed to parse builtin skill");
            }
        }
    }
    skills
}

/// 多来源技能解析器：按 scope 优先级（project > user > builtin）合并多个来源，
/// 同名冲突时高优先级覆盖低优先级。供 runtime 装配技能工具、CLI/TUI `/skills`
/// 展示统一清单使用。
///
/// 来源可以是目录（[`Self::add_source`]，运行时解析）或内存技能列表
/// （[`Self::add_preloaded`]，如 crate 内置技能——其目录路径是编译期
/// `CARGO_MANIFEST_DIR`，运行时不可重算，故以 `load_builtin_skills()` 预载）。
///
/// 语义：低优先级先加，同名按「高 scope 优先、同 scope 后加优先」覆盖。
#[derive(Default)]
pub struct SkillResolver {
    sources: Vec<(SkillScope, SkillSource)>,
}

/// 一个技能来源：目录（惰性加载）或预载列表。
enum SkillSource {
    /// 运行时从目录加载 `.md` 技能文件。
    Dir(PathBuf),
    /// 已解析的技能列表（如内置技能）。
    Preloaded(Vec<Skill>),
}

impl SkillResolver {
    /// 创建空解析器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加一个来源目录（低优先级先加；同 scope 后加者优先）。
    pub fn add_source(mut self, scope: SkillScope, root: impl Into<PathBuf>) -> Self {
        self.sources.push((scope, SkillSource::Dir(root.into())));
        self
    }

    /// 追加一组内存技能（用于 crate 内置等运行时不可重算路径的源）。
    pub fn add_preloaded(mut self, scope: SkillScope, skills: Vec<Skill>) -> Self {
        self.sources.push((scope, SkillSource::Preloaded(skills)));
        self
    }

    /// 已注册的来源数。
    pub fn source_count(&self) -> usize {
        self.sources.len()
    }

    /// 解析：低优先级先加载，同名按「高 scope 优先、同 scope 后加优先」覆盖。
    /// 解析失败（目录不存在 / 含 malformed 文件）由各 loader 内部降级，不阻断。
    pub fn resolve(&self) -> Vec<Skill> {
        // 稳定性：IndexMap 保插入序，resolve 后顺序 = 最终覆盖者出现顺序。
        let mut by_name: indexmap::IndexMap<String, Skill> = indexmap::IndexMap::new();
        for (scope, source) in &self.sources {
            let skills: Vec<Skill> = match source {
                SkillSource::Dir(root) => {
                    let loader = SkillLoader::new(root).with_scope(*scope);
                    match loader.load_all() {
                        Ok(skills) => skills,
                        Err(e) => {
                            tracing::warn!(path = %root.display(), error = %e, "skill source load failed");
                            Vec::new()
                        }
                    }
                }
                SkillSource::Preloaded(skills) => skills.clone(),
            };
            for skill in skills {
                let key = skill.name.clone();
                match by_name.get(&key) {
                    // 仅当既有来源层级更高时保留；同 scope 或更低时由
                    // 后加者（skill）覆盖——同层并列目录后加者优先。
                    Some(existing) if scope_rank(existing.scope) > scope_rank(skill.scope) => {
                        continue;
                    }
                    _ => {
                        by_name.insert(key, skill);
                    }
                }
            }
        }
        by_name.into_values().collect()
    }
}

use deepseeknova_core::registry::{Skill, SkillScope};

/// 优先级排序：project(2) > user(1) > builtin(0)。
/// 孤儿规则禁止为外部类型 `SkillScope` 加 impl 方法，故用内部 free function。
fn scope_rank(scope: SkillScope) -> u8 {
    match scope {
        SkillScope::Builtin => 0,
        SkillScope::User => 1,
        SkillScope::Project => 2,
    }
}

use async_trait::async_trait;
use deepseeknova_core::{Tool, ToolContext, ToolSchema};

// ---------------------------------------------------------------------------
// SkillTool — exposes a Skill as a Tool
// ---------------------------------------------------------------------------

/// Wraps a [`Skill`] so it can be registered in the tool registry.
///
/// When the agent invokes this tool, it returns the skill's system prompt.
/// The agent then incorporates that prompt into its next reasoning step.
pub struct SkillTool {
    skill: Skill,
}

impl SkillTool {
    /// Wrap a [`Skill`] so it can be exposed as a tool. The skill's name,
    /// description and system prompt are read on invocation.
    pub fn new(skill: Skill) -> Self {
        Self { skill }
    }

    /// Return a reference to the underlying skill.
    pub fn skill(&self) -> &Skill {
        &self.skill
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: format!("skill__{}", self.skill.name),
            description: format!(
                "Activate the '{}' skill: {}. Returns the skill's system prompt.",
                self.skill.name, self.skill.description
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    fn read_only(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        _args: &str,
    ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
        let mut output = String::new();
        output.push_str(&format!("# Skill Activated: {}\n\n", self.skill.name));
        output.push_str(&self.skill.system_prompt);

        if !self.skill.tools_allowed.is_empty() {
            output.push_str("\n\n## Allowed Tools\n\n");
            for tool in &self.skill.tools_allowed {
                output.push_str(&format!("- `{tool}`\n"));
            }
        }

        if let Some(ref model) = self.skill.model {
            output.push_str(&format!("\n## Preferred Model\n\n`{model}`\n"));
        }

        Ok(output)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_tool_schema_name_is_prefixed() {
        let skill = Skill {
            name: "code-reviewer".into(),
            description: "Reviews code".into(),
            model: None,
            tools_allowed: vec!["read_file".into()],
            system_prompt: "Be thorough.".into(),
            scope: SkillScope::Project,
        };
        let tool = SkillTool::new(skill);
        let schema = tool.schema();
        assert_eq!(schema.name, "skill__code-reviewer");
        assert!(schema.description.contains("code-reviewer"));
    }

    #[test]
    fn skill_tool_is_read_only() {
        let skill = Skill {
            name: "test".into(),
            description: "...".into(),
            model: None,
            tools_allowed: vec![],
            system_prompt: "...".into(),
            scope: SkillScope::User,
        };
        assert!(SkillTool::new(skill).read_only());
    }

    #[tokio::test]
    async fn skill_tool_execute_returns_prompt() {
        let skill = Skill {
            name: "helper".into(),
            description: "Helps out".into(),
            model: Some("claude-sonnet-5".into()),
            tools_allowed: vec!["grep".into(), "glob".into()],
            system_prompt: "You are a helpful assistant.".into(),
            scope: SkillScope::Builtin,
        };
        let tool = SkillTool::new(skill);
        let ctx = ToolContext::new("call-1");
        let result = tool.execute(&ctx, "{}").await.unwrap();

        assert!(result.contains("Skill Activated: helper"));
        assert!(result.contains("You are a helpful assistant."));
        assert!(result.contains("grep"));
        assert!(result.contains("glob"));
        assert!(result.contains("claude-sonnet-5"));
    }

    // ── SkillResolver 优先级测试 ────────────────────────────────────────

    /// 在 tempdir 下写一个同名/不同名 skill，返回目录路径。
    fn write_skill(dir: &std::path::Path, name: &str, body: &str) {
        let path = dir.join(format!("{name}.md"));
        std::fs::write(
            &path,
            format!("---\nname: {name}\ndescription: {name}\n---\n\n{body}"),
        )
        .unwrap();
    }

    #[test]
    fn resolver_project_overrides_user_and_builtin() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        // 同名 skill：builtin(user 层下无）→ 三层同名的「triage」。
        write_skill(&user, "triage", "user version");
        write_skill(&project, "triage", "project version");

        let resolved = SkillResolver::new()
            .add_source(SkillScope::Project, &project)
            .add_source(SkillScope::User, &user)
            .resolve();
        assert_eq!(resolved.len(), 1, "同名应合并为一条");
        assert_eq!(resolved[0].system_prompt, "project version");
        assert_eq!(resolved[0].scope, SkillScope::Project);
    }

    #[test]
    fn resolver_user_overrides_builtin() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");
        std::fs::create_dir_all(&user).unwrap();
        write_skill(&user, "coding-copilot", "user's coding-copilot");

        // builtin 目录里是真实内置技能；user 同名应覆盖。
        let resolved = SkillResolver::new()
            .add_source(
                SkillScope::Builtin,
                std::path::Path::new(BUILTIN_SKILLS_DIR),
            )
            .add_source(SkillScope::User, &user)
            .resolve();
        let copilot = resolved
            .iter()
            .find(|s| s.name == "coding-copilot")
            .expect("应存在");
        assert_eq!(copilot.scope, SkillScope::User, "user 同名覆盖 builtin");
        assert!(copilot.system_prompt.contains("user's coding-copilot"));
        // 其余 builtin 技能仍在。
        assert!(resolved.iter().any(|s| s.name == "frontend-developer"));
    }

    #[test]
    fn resolver_merges_distinct_names() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        write_skill(&a, "alpha", "A");
        write_skill(&b, "beta", "B");

        let resolved = SkillResolver::new()
            .add_source(SkillScope::User, &a)
            .add_source(SkillScope::Project, &b)
            .resolve();
        let names: Vec<&str> = resolved.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"beta"));
        assert_eq!(resolved.len(), 2);
    }

    #[test]
    fn resolver_same_scope_later_source_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let p1 = tmp.path().join("p1");
        let p2 = tmp.path().join("p2");
        std::fs::create_dir_all(&p1).unwrap();
        std::fs::create_dir_all(&p2).unwrap();
        write_skill(&p1, "dup", "first");
        write_skill(&p2, "dup", "second");

        // 同 scope：后加者（p2）优先。
        let resolved = SkillResolver::new()
            .add_source(SkillScope::Project, &p1)
            .add_source(SkillScope::Project, &p2)
            .resolve();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].system_prompt, "second");
    }

    #[test]
    fn resolver_empty_and_missing_sources() {
        let resolved = SkillResolver::new()
            .add_source(SkillScope::User, "/tmp/__nonexistent_skills__")
            .resolve();
        assert!(resolved.is_empty(), "缺失目录不应报错");
        assert!(SkillResolver::new().resolve().is_empty());
    }
}
