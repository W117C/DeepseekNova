//! # Recall Engine — Cross-session memory recall
//!
//! Automatically retrieves relevant memories at the start of each conversation
//! turn, injecting context from past sessions, skills, and user profile.
//!
//! Inspired by Hermes Agent's "memory nudge" mechanism.

use crate::memory::profile::UserProfile;
use crate::memory::skill::SkillManager;
use crate::memory::store::{MemoryCategory, MemoryStore};
use anyhow::Result;
use std::sync::Arc;
use tracing::debug;

/// Configuration for the recall engine.
#[derive(Debug, Clone)]
pub struct RecallConfig {
    /// Maximum memories to recall per turn.
    pub max_memories: usize,
    /// Maximum skills to match per turn.
    pub max_skills: usize,
    /// Whether to include user profile in context.
    pub include_profile: bool,
}

impl Default for RecallConfig {
    fn default() -> Self {
        Self {
            max_memories: 10,
            max_skills: 3,
            include_profile: true,
        }
    }
}

/// The recall engine combines memory store, skill manager, and user profile
/// to produce a context block injected at the start of each turn.
pub struct RecallEngine {
    store: Arc<MemoryStore>,
    skills: SkillManager,
    profile: UserProfile,
    config: RecallConfig,
}

impl RecallEngine {
    pub fn new(
        store: Arc<MemoryStore>,
        skills: SkillManager,
        profile: UserProfile,
        config: RecallConfig,
    ) -> Self {
        Self {
            store,
            skills,
            profile,
            config,
        }
    }

    /// Recall relevant context for a user query.
    /// Returns a formatted context block to inject into the conversation.
    pub fn recall(&self, query: &str) -> Result<RecallResult> {
        let mut sections = Vec::new();

        // 1. Search past memories (task + skill categories)
        let memories = self.store.search(query, self.config.max_memories)?;
        if !memories.is_empty() {
            let mut lines = vec!["## Relevant Past Memories".to_string()];
            for m in &memories {
                lines.push(format!(
                    "- [{}] {} (score: {:.2})",
                    m.entry.category.as_str(),
                    m.snippet,
                    m.score
                ));
            }
            sections.push(lines.join("\n"));
        }

        // 2. Find matching skills
        let skills = self.skills.find_matching_skills(query);
        if !skills.is_empty() {
            let mut lines = vec!["## Available Skills".to_string()];
            for s in skills.iter().take(self.config.max_skills) {
                lines.push(format!(
                    "- **{}**: {} (used {}x, success rate {:.0}%)",
                    s.frontmatter.name,
                    s.frontmatter.description,
                    s.frontmatter.use_count,
                    if s.frontmatter.use_count > 0 {
                        s.frontmatter.success_count as f64 / s.frontmatter.use_count as f64 * 100.0
                    } else {
                        0.0
                    }
                ));
            }
            sections.push(lines.join("\n"));
        }

        // 3. Include user profile summary
        if self.config.include_profile {
            let profile_summary = self.profile.summary();
            if !profile_summary.is_empty() {
                sections.push(profile_summary);
            }
        }

        Ok(RecallResult {
            context: sections.join("\n\n"),
            memory_count: memories.len(),
            skill_count: skills.len(),
            has_profile: self.config.include_profile && !self.profile.summary().is_empty(),
        })
    }

    /// Store a memory after a conversation turn.
    pub fn remember(
        &self,
        content: impl Into<String>,
        category: MemoryCategory,
        tags: Vec<String>,
        source: impl Into<String>,
        importance: f32,
    ) -> Result<()> {
        let entry = crate::memory::store::make_entry(content, category, tags, source, importance);
        self.store.store(&entry)?;
        debug!(category = entry.category.as_str(), "memory stored");
        Ok(())
    }

    /// Observe a user preference and update the profile.
    pub fn observe_user(
        &mut self,
        key: impl Into<String>,
        value: impl Into<String>,
        category: crate::memory::profile::ProfileCategory,
    ) {
        self.profile.observe(key, value, category);
    }

    /// Get a reference to the user profile.
    pub fn profile(&self) -> &UserProfile {
        &self.profile
    }

    /// Get a mutable reference to the user profile.
    pub fn profile_mut(&mut self) -> &mut UserProfile {
        &mut self.profile
    }
}

/// Result of a recall operation.
#[derive(Debug, Clone)]
pub struct RecallResult {
    /// The context block to inject into the conversation.
    pub context: String,
    /// Number of memories recalled.
    pub memory_count: usize,
    /// Number of skills matched.
    pub skill_count: usize,
    /// Whether user profile was included.
    pub has_profile: bool,
}

impl RecallResult {
    pub fn is_empty(&self) -> bool {
        self.context.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::profile::ProfileCategory;

    #[test]
    fn test_recall() {
        let store = MemoryStore::open_in_memory().unwrap();
        let skills = SkillManager::new(Default::default());
        let profile = UserProfile::new();

        // Store some memories
        let store = Arc::new(store);
        store
            .store(&crate::memory::store::make_entry(
                "Built a Rust web server using axum",
                MemoryCategory::Task,
                vec!["rust".into(), "axum".into()],
                "session-1",
                0.8,
            ))
            .unwrap();

        let mut engine = RecallEngine::new(store.clone(), skills, profile, RecallConfig::default());

        // Observe user preference
        engine.observe_user("language", "Rust", ProfileCategory::LanguagePreference);

        let result = engine.recall("Rust web server").unwrap();
        assert!(result.memory_count > 0, "should recall memories");
        assert!(result.has_profile, "should include profile");
        assert!(
            result.context.contains("Rust"),
            "context should mention Rust"
        );
    }

    #[test]
    fn test_empty_recall() {
        let store = Arc::new(MemoryStore::open_in_memory().unwrap());
        let skills = SkillManager::new(Default::default());
        let profile = UserProfile::new();
        let engine = RecallEngine::new(store, skills, profile, RecallConfig::default());

        let result = engine.recall("test query").unwrap();
        assert_eq!(result.memory_count, 0);
        assert!(!result.has_profile);
    }
}
