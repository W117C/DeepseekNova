//! Skill loader — walks a directory of markdown files with YAML frontmatter
//! and parses them into [`Skill`] structs.
//!
//! ## File format
//!
//! ```markdown
//! ---
//! name: code-reviewer
//! description: Review code for bugs and style issues
//! model: claude-sonnet-5
//! tools_allowed:
//!   - read_file
//!   - grep
//!   - glob
//! ---
//!
//! # Code Reviewer
//!
//! You are a senior software engineer. When reviewing code:
//! 1. Check for correctness first
//! 2. Look for security issues
//! 3. Suggest style improvements
//! ...
//! ```
//!
//! The frontmatter block (between `---` delimiters) is YAML.
//! The body is the system prompt injected when the skill is activated.

use anyhow::Context;
use dpronix_core::registry::Skill;
use serde::Deserialize;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Frontmatter schema
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    name: String,
    description: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    tools_allowed: Vec<String>,
}

// ---------------------------------------------------------------------------
// SkillLoader
// ---------------------------------------------------------------------------

/// Loads skills from a `.dpronix/skills/` directory.
pub struct SkillLoader {
    root: PathBuf,
}

impl SkillLoader {
    /// Create a new loader rooted at `path` (e.g. `~/.dpronix/skills/` or
    /// `<project>/.dpronix/skills/`).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { root: path.into() }
    }

    /// Walk the root directory and return all parsed skills.
    ///
    /// Only `.md` files are considered. Files whose frontmatter fails
    /// to parse are skipped with a warning.
    pub fn load_all(&self) -> anyhow::Result<Vec<Skill>> {
        if !self.root.exists() {
            tracing::debug!("skill directory {:?} does not exist — returning empty", self.root);
            return Ok(Vec::new());
        }

        let mut skills = Vec::new();

        for entry in walkdir::WalkDir::new(&self.root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            match parse_skill_file(path) {
                Ok(skill) => {
                    tracing::debug!(name = %skill.name, path = %path.display(), "loaded skill");
                    skills.push(skill);
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "skipping malformed skill file"
                    );
                }
            }
        }

        tracing::info!(count = skills.len(), "loaded skills from {:?}", self.root);
        Ok(skills)
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Parse a single `.md` skill file.
fn parse_skill_file(path: &Path) -> anyhow::Result<Skill> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read skill file: {}", path.display()))?;

    let (frontmatter_yaml, body) = split_frontmatter(&raw)
        .with_context(|| format!("invalid frontmatter in {}", path.display()))?;

    let fm: SkillFrontmatter = serde_yaml::from_str(&frontmatter_yaml)
        .with_context(|| format!("invalid YAML frontmatter in {}", path.display()))?;

    let body = body.trim().to_string();
    if body.is_empty() {
        anyhow::bail!("skill file {} has empty body", path.display());
    }

    Ok(Skill {
        name: fm.name,
        description: fm.description,
        model: fm.model,
        tools_allowed: fm.tools_allowed,
        system_prompt: body,
    })
}

/// Split raw markdown into (frontmatter_yaml, body).
///
/// Expects the file to start with `---`, followed by YAML on the next line,
/// then `---` on its own line, then the body.
fn split_frontmatter(raw: &str) -> Option<(String, String)> {
    let trimmed = raw.trim_start();

    // Strip opening "---"
    let after_open = trimmed.strip_prefix("---")?;
    // Consume the newline after opening delimiter
    let after_open = after_open
        .strip_prefix('\n')
        .or_else(|| after_open.strip_prefix("\r\n"))
        .unwrap_or(after_open);

    // Find closing "---" on its own line
    let (yaml, body) = after_open.split_once("\n---")?;
    // Consume the newline after closing delimiter
    let body = body
        .strip_prefix('\n')
        .or_else(|| body.strip_prefix("\r\n"))
        .unwrap_or(body);

    Some((yaml.to_string(), body.to_string()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_skill() {
        let raw = "---\nname: test-skill\ndescription: A test skill\n---\n\nYou are a test assistant.";
        let (yaml, body) = split_frontmatter(raw).unwrap();
        assert!(yaml.contains("name: test-skill"));
        assert!(body.contains("You are a test assistant"));
    }

    #[test]
    fn parse_skill_with_tools() {
        let raw = "---\nname: reviewer\ndescription: Code review\ntools_allowed:\n  - read_file\n  - grep\n---\n\nBe thorough.";
        let fm: SkillFrontmatter = serde_yaml::from_str(
            &split_frontmatter(raw).unwrap().0,
        )
        .unwrap();
        assert_eq!(fm.name, "reviewer");
        assert_eq!(fm.tools_allowed, vec!["read_file", "grep"]);
    }

    #[test]
    fn parse_skill_with_model() {
        let raw = "---\nname: planner\ndescription: Plan tasks\nmodel: claude-opus-4-8\n---\n\nPlan carefully.";
        let fm: SkillFrontmatter =
            serde_yaml::from_str(&split_frontmatter(raw).unwrap().0).unwrap();
        assert_eq!(fm.model.unwrap(), "claude-opus-4-8");
    }

    #[test]
    fn split_frontmatter_rejects_no_delimiter() {
        assert!(split_frontmatter("just body\nno frontmatter").is_none());
    }

    #[test]
    fn loader_returns_empty_for_missing_dir() {
        let loader = SkillLoader::new("/tmp/__nonexistent_skills_dir__");
        let skills = loader.load_all().unwrap();
        assert!(skills.is_empty());
    }
}
