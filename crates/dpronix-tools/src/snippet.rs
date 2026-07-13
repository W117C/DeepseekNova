//! # Snippet System
//!
//! Inspired by deepcode-cli's snippet IDs. Tracks file reads and validates
//! that edits reference up-to-date content. Prevents the model from editing
//! stale file views.
//!
//! ## How it works
//!
//! 1. `read_file` returns content + `[SNIPPET:id]` tag
//! 2. `edit_file` accepts optional `snippet_id` parameter
//! 3. If provided, validates the file's current content matches what was read
//! 4. If stale, returns the current content instead of failing

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;

/// A snippet record — tracks a file read for edit validation.
#[derive(Debug, Clone)]
pub struct Snippet {
    pub id: String,
    pub path: String,
    pub content_hash: String,
    pub content_preview: String,
}

/// Global snippet tracker.
#[derive(Default)]
pub struct SnippetTracker {
    snippets: HashMap<String, Snippet>,
    path_to_id: HashMap<String, String>,
}

impl SnippetTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a file read and return a snippet ID.
    pub fn register(&mut self, path: &str, content: &str) -> String {
        let hash = self::hash_content(content);
        let id = format!("snip_{}", &hash[..12]);

        // If this path already has a snippet, update it
        self.snippets.insert(id.clone(), Snippet {
            id: id.clone(),
            path: path.to_string(),
            content_hash: hash,
            content_preview: content.chars().take(100).collect(),
        });
        self.path_to_id.insert(path.to_string(), id.clone());
        info!(path, snippet = %id, "snippet registered");
        id
    }

    /// Validate that a snippet is still current for its file.
    /// Returns `Ok(())` if valid, `Err(current_content)` if stale.
    pub fn validate(&self, snippet_id: &str, current_content: &str) -> Result<(), String> {
        let snippet = self.snippets.get(snippet_id)
            .ok_or_else(|| "unknown snippet".to_string())?;

        let current_hash = hash_content(current_content);
        if current_hash == snippet.content_hash {
            Ok(())
        } else {
            info!(
                path = %snippet.path,
                "snippet stale — file changed since read"
            );
            Err(current_content.to_string())
        }
    }

    /// Clean up old snippets for a path.
    pub fn invalidate(&mut self, path: &str) {
        if let Some(id) = self.path_to_id.remove(path) {
            self.snippets.remove(&id);
        }
    }
}

/// Hash file content for comparison.
pub fn hash_content(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

/// Global singleton snippet tracker.
use std::sync::OnceLock;
static SNIPPET_TRACKER: OnceLock<Arc<Mutex<SnippetTracker>>> = OnceLock::new();

pub fn global_tracker() -> &'static Arc<Mutex<SnippetTracker>> {
    SNIPPET_TRACKER.get_or_init(|| Arc::new(Mutex::new(SnippetTracker::new())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_validate() {
        let mut tracker = SnippetTracker::new();
        let id = tracker.register("test.rs", "hello world");
        assert!(tracker.validate(&id, "hello world").is_ok());
        assert!(tracker.validate(&id, "changed content").is_err());
    }

    #[test]
    fn test_hash_is_deterministic() {
        let h1 = hash_content("same content");
        let h2 = hash_content("same content");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_invalidate() {
        let mut tracker = SnippetTracker::new();
        let id = tracker.register("test.rs", "content");
        tracker.invalidate("test.rs");
        assert!(tracker.validate(&id, "content").is_err());
    }
}
