#![allow(
    clippy::unwrap_used,
    clippy::needless_borrow,
    clippy::needless_borrows_for_generic_args
)]
//! # Memory Store — SQLite + FTS5 backed persistent memory
//!
//! Provides full-text search across all memory entries using SQLite FTS5.
//! Replaces the brute-force vector search with millisecond-level recall.
//!
//! ## Schema
//!
//! ```sql
//! CREATE VIRTUAL TABLE memory_fts USING fts5(
//!   content, tags, category, source,
//!   created_at UNINDEXED, importance UNINDEXED, id UNINDEXED
//! );
//! ```

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::info;

/// A single memory entry stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub content: String,
    pub tags: Vec<String>,
    pub category: MemoryCategory,
    pub source: String,
    pub created_at: i64,
    pub importance: f32,
}

/// Categories for organizing memories (Hermes-inspired four-layer architecture).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCategory {
    /// Short-term: current conversation context (not persisted here).
    ShortTerm,
    /// Task-level: session history and project progress.
    Task,
    /// Long-term: extracted skills and reusable patterns.
    Skill,
    /// Permanent: user profile and preferences.
    UserProfile,
}

impl MemoryCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ShortTerm => "short_term",
            Self::Task => "task",
            Self::Skill => "skill",
            Self::UserProfile => "user_profile",
        }
    }
}

/// Search result from FTS5 query.
#[derive(Debug, Clone)]
pub struct MemorySearchResult {
    pub entry: MemoryEntry,
    pub score: f64,
    pub snippet: String,
}

/// SQLite + FTS5 memory store.
pub struct MemoryStore {
    db: Arc<Mutex<rusqlite::Connection>>,
}

impl MemoryStore {
    /// Open or create a memory database at the given path.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(&parent).ok();
        }
        let db = rusqlite::Connection::open(&path)
            .with_context(|| format!("failed to open memory database at {}", path.display()))?;

        // Enable FTS5 and create tables
        db.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
                content,
                tags,
                category,
                source,
                created_at UNINDEXED,
                importance UNINDEXED,
                id UNINDEXED,
                tokenize = 'porter unicode61'
            );",
        )?;

        // FTS5 doesn't support INSERT OR REPLACE directly; use a delete-then-insert pattern.
        info!(path = %path.display(), "memory store initialized");

        Ok(Self {
            db: Arc::new(Mutex::new(db)),
        })
    }

    /// Open an in-memory database (for tests).
    pub fn open_in_memory() -> Result<Self> {
        let db =
            rusqlite::Connection::open_in_memory().context("failed to open in-memory database")?;
        db.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
                content,
                tags,
                category,
                source,
                created_at UNINDEXED,
                importance UNINDEXED,
                id UNINDEXED,
                tokenize = 'porter unicode61'
            );",
        )?;
        Ok(Self {
            db: Arc::new(Mutex::new(db)),
        })
    }

    /// Store a memory entry.
    pub fn store(&self, entry: &MemoryEntry) -> Result<()> {
        let db = self.db.lock().unwrap();
        let tags_str = entry.tags.join(" ");
        // Delete existing entry with same id first (upsert pattern)
        db.execute(
            "DELETE FROM memory_fts WHERE id = ?1",
            rusqlite::params![&entry.id],
        )?;
        db.execute(
            "INSERT INTO memory_fts (content, tags, category, source, created_at, importance, id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                &entry.content,
                &tags_str,
                entry.category.as_str(),
                &entry.source,
                entry.created_at,
                entry.importance,
                &entry.id,
            ],
        )?;
        Ok(())
    }

    /// Search memories by full-text query. Returns ranked results.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<MemorySearchResult>> {
        let db = self.db.lock().unwrap();

        // Build FTS5 MATCH query — split into tokens and join with OR for broad matching
        let tokens: Vec<String> = query
            .split_whitespace()
            .filter(|t| !t.is_empty())
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect();
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        let safe_query = tokens.join(" OR ");
        let sql = "SELECT id, content, tags, category, source, created_at, importance, bm25(memory_fts) as score
             FROM memory_fts
             WHERE memory_fts MATCH ?
             ORDER BY score
             LIMIT ?";

        let mut stmt = db.prepare(&sql)?;
        let results = stmt
            .query_map(rusqlite::params![safe_query, limit as i64], |row| {
                let id: String = row.get(0)?;
                let content: String = row.get(1)?;
                let tags: String = row.get(2)?;
                let category: String = row.get(3)?;
                let source: String = row.get(4)?;
                let created_at: i64 = row.get(5)?;
                let importance: f64 = row.get(6)?;
                let score: f64 = row.get(7)?;

                let entry = MemoryEntry {
                    id,
                    content,
                    tags: if tags.is_empty() {
                        Vec::new()
                    } else {
                        tags.split(' ').map(|s| s.to_string()).collect()
                    },
                    category: match category.as_str() {
                        "task" => MemoryCategory::Task,
                        "skill" => MemoryCategory::Skill,
                        "user_profile" => MemoryCategory::UserProfile,
                        _ => MemoryCategory::ShortTerm,
                    },
                    source,
                    created_at,
                    importance: importance as f32,
                };

                // Generate snippet
                let snippet = format!(
                    "{:100}",
                    entry.content.chars().take(100).collect::<String>()
                );

                Ok(MemorySearchResult {
                    entry,
                    score: -score, // bm25 returns negative scores (lower = better), invert
                    snippet,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Search within a specific category.
    pub fn search_category(
        &self,
        query: &str,
        category: MemoryCategory,
        limit: usize,
    ) -> Result<Vec<MemorySearchResult>> {
        let db = self.db.lock().unwrap();
        let tokens: Vec<String> = query
            .split_whitespace()
            .filter(|t| !t.is_empty())
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect();
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        let safe_query = tokens.join(" OR ");
        let sql = "SELECT id, content, tags, category, source, created_at, importance, bm25(memory_fts) as score
             FROM memory_fts
             WHERE memory_fts MATCH ? AND category = ?
             ORDER BY score
             LIMIT ?";

        let mut stmt = db.prepare(&sql)?;
        let results = stmt
            .query_map(
                rusqlite::params![safe_query, category.as_str(), limit as i64],
                |row| {
                    let id: String = row.get(0)?;
                    let content: String = row.get(1)?;
                    let tags: String = row.get(2)?;
                    let cat: String = row.get(3)?;
                    let source: String = row.get(4)?;
                    let created_at: i64 = row.get(5)?;
                    let importance: f64 = row.get(6)?;
                    let score: f64 = row.get(7)?;

                    let entry = MemoryEntry {
                        id,
                        content,
                        tags: if tags.is_empty() {
                            Vec::new()
                        } else {
                            tags.split(' ').map(|s| s.to_string()).collect()
                        },
                        category: match cat.as_str() {
                            "task" => MemoryCategory::Task,
                            "skill" => MemoryCategory::Skill,
                            "user_profile" => MemoryCategory::UserProfile,
                            _ => MemoryCategory::ShortTerm,
                        },
                        source,
                        created_at,
                        importance: importance as f32,
                    };

                    Ok(MemorySearchResult {
                        snippet: entry.content.chars().take(100).collect(),
                        entry,
                        score: -score,
                    })
                },
            )?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Delete a memory by ID.
    pub fn delete(&self, id: &str) -> Result<bool> {
        let db = self.db.lock().unwrap();
        let rows = db.execute(
            "DELETE FROM memory_fts WHERE id = ?1",
            rusqlite::params![id],
        )?;
        Ok(rows > 0)
    }

    /// Get all memories in a category.
    pub fn list_category(&self, category: MemoryCategory) -> Result<Vec<MemoryEntry>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare(
            "SELECT id, content, tags, category, source, created_at, importance
             FROM memory_fts
             WHERE category = ?
             ORDER BY created_at DESC",
        )?;

        let results = stmt
            .query_map(rusqlite::params![category.as_str()], |row| {
                let id: String = row.get(0)?;
                let content: String = row.get(1)?;
                let tags: String = row.get(2)?;
                let cat: String = row.get(3)?;
                let source: String = row.get(4)?;
                let created_at: i64 = row.get(5)?;
                let importance: f64 = row.get(6)?;

                Ok(MemoryEntry {
                    id,
                    content,
                    tags: if tags.is_empty() {
                        Vec::new()
                    } else {
                        tags.split(' ').map(|s| s.to_string()).collect()
                    },
                    category: match cat.as_str() {
                        "task" => MemoryCategory::Task,
                        "skill" => MemoryCategory::Skill,
                        "user_profile" => MemoryCategory::UserProfile,
                        _ => MemoryCategory::ShortTerm,
                    },
                    source,
                    created_at,
                    importance: importance as f32,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Count total memories.
    pub fn count(&self) -> Result<usize> {
        let db = self.db.lock().unwrap();
        let count: i64 = db.query_row("SELECT COUNT(*) FROM memory_fts", [], |row| row.get(0))?;
        Ok(count as usize)
    }
}

/// Helper to create a memory entry.
pub fn make_entry(
    content: impl Into<String>,
    category: MemoryCategory,
    tags: Vec<String>,
    source: impl Into<String>,
    importance: f32,
) -> MemoryEntry {
    MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        content: content.into(),
        tags,
        category,
        source: source.into(),
        created_at: Utc::now().timestamp(),
        importance,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_store_and_search() {
        let store = MemoryStore::open_in_memory().unwrap();

        store
            .store(&make_entry(
                "User prefers Rust for systems programming",
                MemoryCategory::UserProfile,
                vec!["preference".into(), "rust".into()],
                "session-1",
                0.9,
            ))
            .unwrap();

        store
            .store(&make_entry(
                "Implemented FTS5 search for memory recall",
                MemoryCategory::Task,
                vec!["fts5".into(), "search".into()],
                "session-2",
                0.8,
            ))
            .unwrap();

        store
            .store(&make_entry(
                "Skill: when building a CLI, use clap with derive macros",
                MemoryCategory::Skill,
                vec!["cli".into(), "clap".into()],
                "auto-extracted",
                0.85,
            ))
            .unwrap();

        let results = store.search("Rust programming", 10).unwrap();
        assert!(!results.is_empty(), "should find results");
        assert_eq!(results[0].entry.category, MemoryCategory::UserProfile);

        let skill_results = store
            .search_category("CLI", MemoryCategory::Skill, 10)
            .unwrap();
        assert!(!skill_results.is_empty());
        assert!(skill_results[0].entry.content.contains("clap"));

        assert_eq!(store.count().unwrap(), 3);
    }

    #[test]
    fn test_delete() {
        let store = MemoryStore::open_in_memory().unwrap();
        let entry = make_entry("test", MemoryCategory::Task, vec![], "test", 0.5);
        store.store(&entry).unwrap();
        assert_eq!(store.count().unwrap(), 1);

        assert!(store.delete(&entry.id).unwrap());
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn test_update_upsert() {
        let store = MemoryStore::open_in_memory().unwrap();
        let mut entry = make_entry(
            "original content",
            MemoryCategory::Task,
            vec![],
            "test",
            0.5,
        );
        store.store(&entry).unwrap();
        assert_eq!(store.count().unwrap(), 1);

        // Update with same ID
        entry.content = "updated content".into();
        store.store(&entry).unwrap();
        assert_eq!(store.count().unwrap(), 1, "upsert should not duplicate");

        let results = store.search("updated", 10).unwrap();
        assert!(!results.is_empty());
    }
}
