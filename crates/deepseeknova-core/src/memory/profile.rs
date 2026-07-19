//! # User Profile — Persistent user preferences and patterns
//!
//! Builds a model of the user over time: preferences, coding style,
//! communication patterns, project context. Stored in the memory store.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::info;

/// A user profile entry — a single fact or preference about the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileEntry {
    pub key: String,
    pub value: String,
    pub category: ProfileCategory,
    pub confidence: f32,
    pub times_observed: u32,
    pub first_seen: i64,
    pub last_seen: i64,
}

/// Categories of user profile information.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ProfileCategory {
    /// Programming language preferences.
    LanguagePreference,
    /// Framework/library preferences.
    FrameworkPreference,
    /// Code style preferences (tabs/spaces, naming conventions).
    CodeStyle,
    /// Communication style (verbose, concise, technical level).
    CommunicationStyle,
    /// Project context (what the user is working on).
    ProjectContext,
    /// Work habits (testing approach, deployment prefs).
    WorkHabit,
    /// Skill level (beginner, intermediate, expert).
    SkillLevel,
    /// Custom category.
    Other,
}

impl ProfileCategory {
    pub fn as_str(&self) -> &str {
        match self {
            Self::LanguagePreference => "language_preference",
            Self::FrameworkPreference => "framework_preference",
            Self::CodeStyle => "code_style",
            Self::CommunicationStyle => "communication_style",
            Self::ProjectContext => "project_context",
            Self::WorkHabit => "work_habit",
            Self::SkillLevel => "skill_level",
            Self::Other => "other",
        }
    }
}

/// User profile manager.
pub struct UserProfile {
    entries: HashMap<String, ProfileEntry>,
}

impl UserProfile {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Record an observation about the user.
    /// If the entry already exists, increment times_observed and update confidence.
    pub fn observe(
        &mut self,
        key: impl Into<String>,
        value: impl Into<String>,
        category: ProfileCategory,
    ) {
        let key = key.into();
        let now = chrono::Utc::now().timestamp();

        match self.entries.get_mut(&key) {
            Some(entry) => {
                entry.times_observed += 1;
                entry.last_seen = now;
                // Increase confidence with repeated observations, capped at 1.0
                entry.confidence = (entry.confidence + 0.15).min(1.0);
                // Update value if changed
                let new_value = value.into();
                if entry.value != new_value {
                    info!(key = %entry.key, old = %entry.value, new = %new_value, "profile entry updated");
                    entry.value = new_value;
                }
            }
            None => {
                let value = value.into();
                info!(key = %key, value = %value, category = %category.as_str(), "new profile entry");
                self.entries.insert(
                    key.clone(),
                    ProfileEntry {
                        key,
                        value,
                        category,
                        confidence: 0.3,
                        times_observed: 1,
                        first_seen: now,
                        last_seen: now,
                    },
                );
            }
        }
    }

    /// Get a profile entry by key.
    pub fn get(&self, key: &str) -> Option<&ProfileEntry> {
        self.entries.get(key)
    }

    /// Get all entries in a category.
    pub fn get_category(&self, category: &ProfileCategory) -> Vec<&ProfileEntry> {
        self.entries
            .values()
            .filter(|e| &e.category == category)
            .collect()
    }

    /// Generate a summary of the user profile for injection into context.
    pub fn summary(&self) -> String {
        if self.entries.is_empty() {
            return String::new();
        }

        let mut lines = Vec::new();
        lines.push("# User Profile".to_string());
        lines.push(String::new());

        // Group by category
        let categories = [
            ProfileCategory::LanguagePreference,
            ProfileCategory::FrameworkPreference,
            ProfileCategory::CodeStyle,
            ProfileCategory::CommunicationStyle,
            ProfileCategory::ProjectContext,
            ProfileCategory::WorkHabit,
            ProfileCategory::SkillLevel,
            ProfileCategory::Other,
        ];

        for cat in &categories {
            let entries = self.get_category(cat);
            if entries.is_empty() {
                continue;
            }
            let cat_name = cat.as_str().replace('_', " ");
            lines.push(format!("## {}", cat_name));
            for entry in entries {
                let stars = "★".repeat((entry.confidence * 5.0) as usize);
                lines.push(format!(
                    "- {} (confidence: {} [{}], observed {}x)",
                    entry.value, entry.confidence, stars, entry.times_observed
                ));
            }
            lines.push(String::new());
        }

        lines.join("\n")
    }

    /// Get high-confidence entries only (>= 0.5).
    pub fn high_confidence(&self) -> Vec<&ProfileEntry> {
        self.entries
            .values()
            .filter(|e| e.confidence >= 0.5)
            .collect()
    }

    /// Export to a serializable format.
    pub fn export(&self) -> Vec<ProfileEntry> {
        self.entries.values().cloned().collect()
    }

    /// Import from a list of entries.
    pub fn import(&mut self, entries: Vec<ProfileEntry>) {
        self.entries = entries.into_iter().map(|e| (e.key.clone(), e)).collect();
    }
}

impl Default for UserProfile {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_observe_and_get() {
        let mut profile = UserProfile::new();
        profile.observe("language", "Rust", ProfileCategory::LanguagePreference);
        profile.observe("language", "Rust", ProfileCategory::LanguagePreference);
        profile.observe("language", "Rust", ProfileCategory::LanguagePreference);

        let entry = profile.get("language").unwrap();
        assert_eq!(entry.value, "Rust");
        assert_eq!(entry.times_observed, 3);
        assert!(entry.confidence > 0.3, "confidence should increase");
    }

    #[test]
    fn test_update_value() {
        let mut profile = UserProfile::new();
        profile.observe("language", "Python", ProfileCategory::LanguagePreference);
        profile.observe("language", "Rust", ProfileCategory::LanguagePreference);

        let entry = profile.get("language").unwrap();
        assert_eq!(entry.value, "Rust", "value should update to latest");
        assert_eq!(entry.times_observed, 2);
    }

    #[test]
    fn test_summary() {
        let mut profile = UserProfile::new();
        profile.observe("language", "Rust", ProfileCategory::LanguagePreference);
        profile.observe("editor", "Vim", ProfileCategory::WorkHabit);

        let summary = profile.summary();
        assert!(summary.contains("Rust"));
        assert!(summary.contains("Vim"));
        assert!(summary.contains("User Profile"));
    }

    #[test]
    fn test_high_confidence() {
        let mut profile = UserProfile::new();
        profile.observe("a", "1", ProfileCategory::Other);
        // Observe "b" multiple times to boost confidence
        for _ in 0..5 {
            profile.observe("b", "2", ProfileCategory::Other);
        }

        let high = profile.high_confidence();
        assert_eq!(high.len(), 1);
        assert_eq!(high[0].key, "b");
    }

    #[test]
    fn test_export_import() {
        let mut profile = UserProfile::new();
        profile.observe("language", "Rust", ProfileCategory::LanguagePreference);
        profile.observe("editor", "Vim", ProfileCategory::WorkHabit);

        let exported = profile.export();
        let mut imported = UserProfile::new();
        imported.import(exported);

        assert!(imported.get("language").is_some());
        assert!(imported.get("editor").is_some());
    }
}
