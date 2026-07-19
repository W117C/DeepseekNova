//! # Memory Distillation
//!
//! After completing a project, extract reusable experience and store it
//! in the long-term memory system. This is the "Distill" phase of the
//! DNA Spec (Understand → Plan → Execute → Verify → **Distill**).
//!
//! ## What gets distilled?
//!
//! - User preferences (coding style, tool choices, communication style)
//! - Effective patterns (what worked well)
//! - Failure lessons (what didn't work and why)
//! - Project context (for continuity in future projects)
//! - Skill candidates (complex tasks that could become reusable skills)

#![allow(clippy::needless_borrows_for_generic_args)]

use crate::memory::profile::{ProfileCategory, UserProfile};
use crate::memory::skill::{Skill, SkillFrontmatter, TaskObservation, TaskOutcome};
use crate::memory::store::{MemoryCategory, MemoryStore};
use anyhow::Result;
use chrono::Utc;
use std::sync::Arc;
use tracing::info;

/// Input for the distillation process — collected at project completion.
#[derive(Debug, Clone)]
pub struct ProjectCompletion {
    /// Project name/identifier.
    pub project_name: String,
    /// Session ID where the project was completed.
    pub session_id: String,
    /// Summary of what was accomplished.
    pub summary: String,
    /// Key decisions made during the project.
    pub decisions: Vec<String>,
    /// Technologies/libraries used.
    pub tech_stack: Vec<String>,
    /// What worked well.
    pub what_worked: Vec<String>,
    /// What didn't work (lessons learned).
    pub what_failed: Vec<String>,
    /// User's coding style observations.
    pub style_observations: Vec<StyleObservation>,
    /// Whether the user confirmed distillation.
    pub user_confirmed: bool,
    /// Task observation for skill extraction.
    pub task_observation: Option<TaskObservation>,
}

/// A user style observation.
#[derive(Debug, Clone)]
pub struct StyleObservation {
    pub category: ProfileCategory,
    pub key: String,
    pub value: String,
}

/// The distillation engine.
pub struct DistillationEngine {
    store: Arc<MemoryStore>,
    profile: UserProfile,
}

impl DistillationEngine {
    pub fn new(store: Arc<MemoryStore>, profile: UserProfile) -> Self {
        Self { store, profile }
    }

    /// Run the full distillation process.
    /// Returns a summary of what was stored.
    pub fn distill(&mut self, completion: &ProjectCompletion) -> Result<DistillationResult> {
        if !completion.user_confirmed {
            info!("distillation skipped — user did not confirm");
            return Ok(DistillationResult::skipped());
        }

        let mut memories_stored = 0;
        let mut profile_updates = 0;
        let mut skill_created = false;

        // 1. Store project summary as a task memory
        self.store.store(&crate::memory::store::make_entry(
            format!(
                "Project: {}\nSummary: {}\nTech: {}\nDecisions: {}",
                completion.project_name,
                completion.summary,
                completion.tech_stack.join(", "),
                completion.decisions.join("; ")
            ),
            MemoryCategory::Task,
            vec!["project".into(), "summary".into()],
            &completion.session_id,
            0.8,
        ))?;
        memories_stored += 1;

        // 2. Store what worked
        for insight in &completion.what_worked {
            self.store.store(&crate::memory::store::make_entry(
                format!("[{}] ✅ {}", completion.project_name, insight),
                MemoryCategory::Skill,
                vec!["success".into(), "pattern".into()],
                &completion.session_id,
                0.7,
            ))?;
            memories_stored += 1;
        }

        // 3. Store what failed (lessons learned)
        for lesson in &completion.what_failed {
            self.store.store(&crate::memory::store::make_entry(
                format!("[{}] ❌ {}", completion.project_name, lesson),
                MemoryCategory::Skill,
                vec!["failure".into(), "lesson".into()],
                &completion.session_id,
                0.8,
            ))?;
            memories_stored += 1;
        }

        // 4. Update user profile
        for obs in &completion.style_observations {
            self.profile
                .observe(&obs.key, &obs.value, obs.category.clone());
            profile_updates += 1;
        }

        // 5. Store tech stack preferences
        for tech in &completion.tech_stack {
            self.profile.observe(
                &format!("tech/{}", tech.to_lowercase()),
                tech,
                ProfileCategory::FrameworkPreference,
            );
            profile_updates += 1;
        }

        // 6. Check if a skill should be extracted
        if let Some(ref task_obs) = completion.task_observation {
            if task_obs.outcome != TaskOutcome::Failure && task_obs.tool_calls.len() >= 5 {
                skill_created = true;
                info!(
                    project = %completion.project_name,
                    "skill extraction candidate identified"
                );
            }
        }

        info!(
            project = %completion.project_name,
            memories_stored,
            profile_updates,
            skill_created,
            "distillation complete"
        );

        Ok(DistillationResult {
            memories_stored,
            profile_updates,
            skill_created,
            project_name: completion.project_name.clone(),
        })
    }

    /// Get a reference to the user profile.
    pub fn profile(&self) -> &UserProfile {
        &self.profile
    }

    /// Generate a skill from a project completion.
    pub fn create_skill_from_project(
        project_name: &str,
        skill_body: &str,
        tags: Vec<String>,
        triggers: Vec<String>,
        session_id: &str,
    ) -> Skill {
        let now = Utc::now().to_rfc3339();
        let slug = project_name.to_lowercase().replace(' ', "-");

        Skill {
            frontmatter: SkillFrontmatter {
                name: format!("project-{}", slug),
                version: "0.1.0".into(),
                description: format!("Auto-extracted skill from project: {}", project_name),
                triggers,
                tags,
                created_at: now.clone(),
                updated_at: now,
                use_count: 0,
                success_count: 0,
                source_session: Some(session_id.to_string()),
            },
            body: skill_body.to_string(),
        }
    }
}

/// Result of the distillation process.
#[derive(Debug, Clone)]
pub struct DistillationResult {
    pub project_name: String,
    pub memories_stored: usize,
    pub profile_updates: usize,
    pub skill_created: bool,
}

impl DistillationResult {
    pub fn skipped() -> Self {
        Self {
            project_name: String::new(),
            memories_stored: 0,
            profile_updates: 0,
            skill_created: false,
        }
    }

    pub fn was_distilled(&self) -> bool {
        self.memories_stored > 0 || self.profile_updates > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_distillation() {
        let store = Arc::new(MemoryStore::open_in_memory().unwrap());
        let profile = UserProfile::new();
        let mut engine = DistillationEngine::new(store, profile);

        let completion = ProjectCompletion {
            project_name: "deepseeknova-memory".into(),
            session_id: "s1".into(),
            summary: "Implemented FTS5-based memory system".into(),
            decisions: vec!["Use SQLite FTS5".into(), "BM25 ranking".into()],
            tech_stack: vec!["Rust".into(), "SQLite".into(), "FTS5".into()],
            what_worked: vec!["FTS5 gives millisecond recall".into()],
            what_failed: vec!["Porter tokenizer doesn't handle CJK well".into()],
            style_observations: vec![StyleObservation {
                category: ProfileCategory::LanguagePreference,
                key: "language".into(),
                value: "Rust".into(),
            }],
            user_confirmed: true,
            task_observation: Some(TaskObservation {
                task_description: "Build memory system".into(),
                tool_calls: vec!["write_file".into(); 6],
                steps_taken: vec!["design".into(), "implement".into(), "test".into()],
                outcome: TaskOutcome::Success,
                user_feedback: None,
                session_id: "s1".into(),
            }),
        };

        let result = engine.distill(&completion).unwrap();
        assert!(result.was_distilled());
        assert_eq!(result.memories_stored, 3); // summary + 1 worked + 1 failed
        assert_eq!(result.profile_updates, 4); // 1 style + 3 tech
        assert!(result.skill_created);

        // Verify profile was updated
        assert!(engine.profile().get("language").is_some());
        assert!(engine.profile().get("tech/rust").is_some());
    }

    #[test]
    fn test_skipped_distillation() {
        let store = Arc::new(MemoryStore::open_in_memory().unwrap());
        let profile = UserProfile::new();
        let mut engine = DistillationEngine::new(store, profile);

        let completion = ProjectCompletion {
            project_name: "test".into(),
            session_id: "s1".into(),
            summary: "Test".into(),
            decisions: vec![],
            tech_stack: vec![],
            what_worked: vec![],
            what_failed: vec![],
            style_observations: vec![],
            user_confirmed: false,
            task_observation: None,
        };

        let result = engine.distill(&completion).unwrap();
        assert!(!result.was_distilled());
    }

    #[test]
    fn test_skill_creation_from_project() {
        let skill = DistillationEngine::create_skill_from_project(
            "Memory System",
            "# Memory System\n\nUse FTS5 for search.",
            vec!["memory".into(), "fts5".into()],
            vec!["build memory".into()],
            "session-1",
        );
        assert_eq!(skill.frontmatter.name, "project-memory-system");
        assert!(skill.frontmatter.description.contains("Memory System"));
        assert_eq!(skill.frontmatter.tags.len(), 2);
    }
}
