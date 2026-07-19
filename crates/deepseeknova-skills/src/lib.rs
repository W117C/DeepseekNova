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

mod loader;

pub use loader::SkillLoader;

/// Path to the built-in skills bundled with this crate.
pub const BUILTIN_SKILLS_DIR: &str = "builtin";

/// Load all built-in skills shipped with the deepseeknova-skills crate.
///
/// These are the default cognitive frameworks that every DeepseekNova
/// agent starts with:
/// - `frontend-developer` — UI/UX design and code generation
/// - `coding-copilot` — multi-language coding assistant
/// - `loop-engineering` — iterative improvement loop
/// - `first-principles` — first-principles reasoning
/// - `adversarial-review` — hostile red-team review
pub fn load_builtin_skills() -> Vec<Skill> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let builtin_path = std::path::Path::new(manifest_dir).join(BUILTIN_SKILLS_DIR);
    let loader = SkillLoader::new(&builtin_path);
    loader.load_all().unwrap_or_else(|e| {
        tracing::warn!(error = %e, "failed to load builtin skills");
        Vec::new()
    })
}

use async_trait::async_trait;
use deepseeknova_core::registry::Skill;
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

    async fn execute(&self, _ctx: &ToolContext, _args: &str) -> anyhow::Result<String> {
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
}
