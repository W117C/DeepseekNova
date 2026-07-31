#![allow(clippy::needless_borrow, clippy::needless_borrows_for_generic_args)]
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

use crate::memory::lifecycle::{LifecycleMeta, MemoryLifecycleStage};
use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
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

/// 持久化的 lifecycle 元数据行（伴随 memory_fts.id）。
#[derive(Debug, Clone)]
pub struct MetaRow {
    pub stage: String,
    pub recall_count: u32,
    pub last_recalled_at: Option<i64>,
    pub embed_dim: Option<i64>,
    pub embed_model: Option<String>,
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

        db.busy_timeout(Duration::from_secs(5))?;
        let _ = db.pragma_update(None, "journal_mode", "WAL");

        // Enable FTS5 and create tables
        db.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
                content, tags, category, source,
                created_at UNINDEXED, importance UNINDEXED, id UNINDEXED,
                tokenize = 'porter unicode61'
            );
            CREATE TABLE IF NOT EXISTS memory_meta(
                id TEXT PRIMARY KEY,
                stage TEXT NOT NULL DEFAULT 'candidate',
                recall_count INTEGER NOT NULL DEFAULT 0,
                last_recalled_at INTEGER,
                embedding BLOB,
                embed_dim INTEGER,
                embed_model TEXT,
                created_at INTEGER NOT NULL DEFAULT 0,
                importance REAL NOT NULL DEFAULT 0.5
            );
            CREATE TABLE IF NOT EXISTS counters(name TEXT PRIMARY KEY, value INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS distill_log(day TEXT PRIMARY KEY, count INTEGER NOT NULL DEFAULT 0);",
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
                content, tags, category, source,
                created_at UNINDEXED, importance UNINDEXED, id UNINDEXED,
                tokenize = 'porter unicode61'
            );
            CREATE TABLE IF NOT EXISTS memory_meta(
                id TEXT PRIMARY KEY,
                stage TEXT NOT NULL DEFAULT 'candidate',
                recall_count INTEGER NOT NULL DEFAULT 0,
                last_recalled_at INTEGER,
                embedding BLOB,
                embed_dim INTEGER,
                embed_model TEXT,
                created_at INTEGER NOT NULL DEFAULT 0,
                importance REAL NOT NULL DEFAULT 0.5
            );
            CREATE TABLE IF NOT EXISTS counters(name TEXT PRIMARY KEY, value INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS distill_log(day TEXT PRIMARY KEY, count INTEGER NOT NULL DEFAULT 0);",
        )?;
        Ok(Self {
            db: Arc::new(Mutex::new(db)),
        })
    }

    /// Store a memory entry.
    pub fn store(&self, entry: &MemoryEntry) -> Result<()> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
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
        db.execute(
            "INSERT INTO memory_meta (id, stage, recall_count, created_at, importance)
             VALUES (?1, 'candidate', 0, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET importance = excluded.importance",
            rusqlite::params![&entry.id, entry.created_at, entry.importance],
        )?;
        Ok(())
    }

    /// Search memories by full-text query. Returns ranked results.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<MemorySearchResult>> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());

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
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
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
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let rows = db.execute(
            "DELETE FROM memory_fts WHERE id = ?1",
            rusqlite::params![id],
        )?;
        // 同步清理 lifecycle 伴行，避免孤儿 meta 积累与状态不一致。
        db.execute(
            "DELETE FROM memory_meta WHERE id = ?1",
            rusqlite::params![id],
        )?;
        Ok(rows > 0)
    }

    /// Get all memories in a category.
    pub fn list_category(&self, category: MemoryCategory) -> Result<Vec<MemoryEntry>> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
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
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let count: i64 = db.query_row("SELECT COUNT(*) FROM memory_fts", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    /// 读取某条记忆的 lifecycle 元数据行。
    pub fn meta(&self, id: &str) -> Result<Option<MetaRow>> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let row = db
            .query_row(
                "SELECT stage, recall_count, last_recalled_at, embed_dim, embed_model
                 FROM memory_meta WHERE id = ?1",
                rusqlite::params![id],
                |r| {
                    Ok(MetaRow {
                        stage: r.get(0)?,
                        recall_count: r.get::<_, i64>(1)? as u32,
                        last_recalled_at: r.get(2)?,
                        embed_dim: r.get(3)?,
                        embed_model: r.get(4)?,
                    })
                },
            )
            .ok();
        Ok(row)
    }

    /// 记一次召回：recall_count++、更新时间、按 lifecycle 规则重算 stage 并持久化。
    pub fn record_recall(&self, id: &str) -> Result<()> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let now = Utc::now().timestamp();
        let (stage_s, count, created_at, importance): (String, i64, i64, f64) = match db.query_row(
            "SELECT stage, recall_count, created_at, importance FROM memory_meta WHERE id = ?1",
            rusqlite::params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        ) {
            Ok(v) => v,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(()), // 无 meta 行则跳过
            Err(e) => return Err(e.into()), // 其它 DB 错误向上传播，不静默吞掉
        };
        let mut meta = LifecycleMeta {
            stage: MemoryLifecycleStage::parse(&stage_s),
            recall_count: count as u32,
            last_recalled_at: Some(now),
            created_at,
            importance: importance as f32,
        };
        meta.record_recall();
        db.execute(
            "UPDATE memory_meta SET stage = ?1, recall_count = ?2, last_recalled_at = ?3 WHERE id = ?4",
            rusqlite::params![meta.stage.as_str(), meta.recall_count as i64, now, id],
        )?;
        Ok(())
    }

    /// 今日已沉淀次数（UTC 日）。
    pub fn distill_count_today(&self) -> Result<u32> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let day = Utc::now().format("%Y-%m-%d").to_string();
        let n: i64 = db
            .query_row(
                "SELECT count FROM distill_log WHERE day = ?1",
                rusqlite::params![day],
                |r| r.get(0),
            )
            .unwrap_or(0);
        Ok(n as u32)
    }

    /// 今日沉淀计数 +1。
    pub fn bump_distill_count(&self) -> Result<()> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let day = Utc::now().format("%Y-%m-%d").to_string();
        db.execute(
            "INSERT INTO distill_log(day, count) VALUES (?1, 1)
             ON CONFLICT(day) DO UPDATE SET count = count + 1",
            rusqlite::params![day],
        )?;
        Ok(())
    }

    /// 记一次召回调用（命中率统计）。
    pub fn note_recall(&self, nonempty: bool) -> Result<()> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        db.execute(
            "INSERT INTO counters(name, value) VALUES ('recall_calls', 1)
             ON CONFLICT(name) DO UPDATE SET value = value + 1",
            [],
        )?;
        if nonempty {
            db.execute(
                "INSERT INTO counters(name, value) VALUES ('recall_nonempty', 1)
                 ON CONFLICT(name) DO UPDATE SET value = value + 1",
                [],
            )?;
        }
        Ok(())
    }

    /// 返回 (recall_calls, recall_nonempty)。
    pub fn recall_counters(&self) -> Result<(u64, u64)> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let get = |name: &str| -> i64 {
            db.query_row(
                "SELECT value FROM counters WHERE name = ?1",
                rusqlite::params![name],
                |r| r.get(0),
            )
            .unwrap_or(0)
        };
        Ok((get("recall_calls") as u64, get("recall_nonempty") as u64))
    }

    /// 泛化计数器：任意名字 +1（B3 审查指标等）。
    pub fn bump_counter(&self, name: &str) -> Result<()> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        db.execute(
            "INSERT INTO counters(name, value) VALUES (?1, 1)
             ON CONFLICT(name) DO UPDATE SET value = value + 1",
            rusqlite::params![name],
        )?;
        Ok(())
    }

    /// 读取泛化计数器（缺失 = 0）。
    pub fn read_counter(&self, name: &str) -> Result<u64> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let v: i64 = db
            .query_row(
                "SELECT value FROM counters WHERE name = ?1",
                rusqlite::params![name],
                |r| r.get(0),
            )
            .unwrap_or(0);
        Ok(v as u64)
    }

    /// 统计 auto-distill 来源条目中已达 verified/permanent 的比例（reinforce 比例）。
    pub fn reinforce_ratio(&self) -> Result<f64> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let total: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM memory_fts WHERE source = 'auto-distill'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if total == 0 {
            return Ok(0.0);
        }
        let reinforced: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM memory_meta m JOIN memory_fts f ON m.id = f.id
                 WHERE f.source = 'auto-distill' AND m.recall_count >= 1",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        Ok(reinforced as f64 / total as f64)
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

    #[test]
    fn delete_removes_meta_row_too() {
        // 回归：删除记忆时同步清理 lifecycle 伴行，不留孤儿。
        let store = MemoryStore::open_in_memory().unwrap();
        let e = make_entry("ephemeral", MemoryCategory::Task, vec![], "t", 0.5);
        store.store(&e).unwrap();
        assert!(store.meta(&e.id).unwrap().is_some());
        assert!(store.delete(&e.id).unwrap());
        assert!(store.meta(&e.id).unwrap().is_none(), "meta must be cleaned");
    }

    #[test]
    fn store_creates_meta_row_as_candidate() {
        let store = MemoryStore::open_in_memory().unwrap();
        let e = make_entry("hello world", MemoryCategory::Task, vec![], "t", 0.5);
        store.store(&e).unwrap();
        let meta = store.meta(&e.id).unwrap().expect("meta row exists");
        assert_eq!(meta.stage, "candidate");
        assert_eq!(meta.recall_count, 0);
    }

    #[test]
    fn record_recall_promotes_and_persists_count() {
        let store = MemoryStore::open_in_memory().unwrap();
        let e = make_entry("promote me", MemoryCategory::Task, vec![], "t", 0.5);
        store.store(&e).unwrap();
        store.record_recall(&e.id).unwrap();
        let meta = store.meta(&e.id).unwrap().unwrap();
        assert_eq!(meta.recall_count, 1);
        assert_eq!(meta.stage, "verified");
    }

    #[test]
    fn distill_counter_increments_per_day() {
        let store = MemoryStore::open_in_memory().unwrap();
        assert_eq!(store.distill_count_today().unwrap(), 0);
        store.bump_distill_count().unwrap();
        store.bump_distill_count().unwrap();
        assert_eq!(store.distill_count_today().unwrap(), 2);
    }

    #[test]
    fn recall_counters_track_hit_rate() {
        let store = MemoryStore::open_in_memory().unwrap();
        store.note_recall(true).unwrap();
        store.note_recall(false).unwrap();
        store.note_recall(true).unwrap();
        let (calls, nonempty) = store.recall_counters().unwrap();
        assert_eq!(calls, 3);
        assert_eq!(nonempty, 2);
    }

    #[test]
    fn generic_counters_bump_and_read() {
        let store = MemoryStore::open_in_memory().unwrap();
        assert_eq!(store.read_counter("review_triggered").unwrap(), 0);
        store.bump_counter("review_triggered").unwrap();
        store.bump_counter("review_triggered").unwrap();
        assert_eq!(store.read_counter("review_triggered").unwrap(), 2);
    }
}
