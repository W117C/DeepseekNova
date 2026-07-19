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

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::info;

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
}

impl Default for SkillExtractionConfig {
    fn default() -> Self {
        Self {
            min_tool_calls: 5,
            min_steps: 3,
            skill_dir: PathBuf::from(".deepseeknova/skills"),
        }
    }
}

/// The skill manager handles extraction, storage, and retrieval.
pub struct SkillManager {
    config: SkillExtractionConfig,
    /// In-memory cache of loaded skills.
    skills: HashMap<String, Skill>,
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

    /// Load all skills from the skill directory.
    fn load_skills(&mut self) -> anyhow::Result<()> {
        if !self.config.skill_dir.exists() {
            return Ok(());
        }
        for entry in std::fs::read_dir(&self.config.skill_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map(|e| e == "md").unwrap_or(false) {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Some(skill) = Skill::from_markdown(&content) {
                        let name = skill.frontmatter.name.clone();
                        self.skills.insert(name, skill);
                    }
                }
            }
        }
        info!(count = self.skills.len(), "loaded skills");
        Ok(())
    }

    /// Evaluate whether a task observation warrants skill extraction.
    pub fn should_extract_skill(&self, obs: &TaskObservation) -> bool {
        obs.tool_calls.len() >= self.config.min_tool_calls
            && obs.steps_taken.len() >= self.config.min_steps
            && obs.outcome != TaskOutcome::Failure
    }

    /// Create a skill from a task observation.
    /// The actual content extraction is done by the LLM — this handles storage.
    pub fn create_skill(&mut self, skill: Skill) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.config.skill_dir)?;
        let filename = format!(
            "{}/{}.md",
            self.config.skill_dir.display(),
            skill.frontmatter.name.replace(' ', "-").to_lowercase()
        );
        std::fs::write(&filename, skill.to_markdown())?;
        info!(name = %skill.frontmatter.name, path = %filename, "skill created");
        self.skills.insert(skill.frontmatter.name.clone(), skill);
        Ok(())
    }

    /// Update skill usage statistics.
    pub fn record_use(&mut self, skill_name: &str, success: bool) -> anyhow::Result<()> {
        let Some(skill) = self.skills.get_mut(skill_name) else {
            return Ok(());
        };
        skill.frontmatter.use_count += 1;
        if success {
            skill.frontmatter.success_count += 1;
        }
        skill.frontmatter.updated_at = chrono::Utc::now().to_rfc3339();
        // Persist
        let filename = format!(
            "{}/{}.md",
            self.config.skill_dir.display(),
            skill.frontmatter.name.replace(' ', "-").to_lowercase()
        );
        std::fs::write(&filename, skill.to_markdown())?;
        Ok(())
    }

    /// Find skills matching a query (by name, tags, or triggers).
    pub fn find_matching_skills(&self, query: &str) -> Vec<&Skill> {
        let query_lower = query.to_lowercase();
        self.skills
            .values()
            .filter(|s| {
                let name_match = s.frontmatter.name.to_lowercase().contains(&query_lower);
                let tag_match = s
                    .frontmatter
                    .tags
                    .iter()
                    .any(|t| t.to_lowercase().contains(&query_lower));
                let trigger_match = s
                    .frontmatter
                    .triggers
                    .iter()
                    .any(|t| t.to_lowercase().contains(&query_lower));
                let body_match = s.body.to_lowercase().contains(&query_lower);
                name_match || tag_match || trigger_match || body_match
            })
            .collect()
    }

    /// Get all skills.
    pub fn list_skills(&self) -> Vec<&Skill> {
        self.skills.values().collect()
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
        };
        assert!(manager.should_extract_skill(&obs));

        let obs_small = TaskObservation {
            task_description: "Quick question".into(),
            tool_calls: vec!["read_file".into()],
            steps_taken: vec!["read".into()],
            outcome: TaskOutcome::Success,
            user_feedback: None,
            session_id: "s1".into(),
        };
        assert!(!manager.should_extract_skill(&obs_small));

        let obs_fail = TaskObservation {
            task_description: "Failed task".into(),
            tool_calls: vec!["write_file".into(); 10],
            steps_taken: vec!["step1".into(); 5],
            outcome: TaskOutcome::Failure,
            user_feedback: None,
            session_id: "s1".into(),
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
}
