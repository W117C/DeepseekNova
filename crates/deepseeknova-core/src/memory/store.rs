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

use crate::memory::embedding::EmbeddingProvider;
use crate::memory::lifecycle::{LifecycleMeta, MemoryLifecycleStage};
use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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

/// 建表 SQL：unicode61 主表（拉丁/代码）+ trigram 辅助表（CJK 子串检索）。
const MEMORY_SCHEMA_SQL: &str = "CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
                content, tags, category, source,
                created_at UNINDEXED, importance UNINDEXED, id UNINDEXED,
                tokenize = 'porter unicode61'
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts_cjk USING fts5(
                content, tags, category, source,
                created_at UNINDEXED, importance UNINDEXED, id UNINDEXED,
                tokenize = 'trigram'
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
            CREATE TABLE IF NOT EXISTS distill_log(day TEXT PRIMARY KEY, count INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO meta(key, value) VALUES('schema_version', '1')
                ON CONFLICT(key) DO NOTHING;";

/// 当前 memory schema 版本：初值 "1"（graph 先例 SCHEMA_VERSION=4）。
/// 版本不符时按迁移表（当前为空）升级，无破坏性变更则只回写版本号。
const MEMORY_SCHEMA_VERSION: &str = "1";

/// 生命周期融合权重默认值（对齐配置 `[memory] rank_lifecycle_weight` 默认 0.3；
/// 0 = 纯 bm25，与旧行为等价）。
pub const DEFAULT_RANK_WEIGHT: f64 = 0.3;

/// schema 版本核对：
/// - 库内版本 == 当前版本：无操作；
/// - 库内版本为**已知旧版本**（可解析为数字且 < 当前版本）：走迁移表（当前为空，空跑）并回写当前版本；
/// - 库内版本 **> 当前版本**（如 '999'，未来版本）：**不回写**，保持原版本号、只读可用
///   （避免旧二进制打开未来库后把版本标记降级抹除，破坏版本簿记）；
/// - 无法解析的未知版本：同样不回写（保守只读）。
fn ensure_schema_version(db: &rusqlite::Connection) -> Result<()> {
    let version: Option<String> = db
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    let downgrade_ok = match version.as_deref() {
        None => true, // 无版本标记（旧库升级路径）→ 写当前版本
        Some(v) => match v.parse::<i64>() {
            // 仅已知旧版本（可解析且 < 当前）走迁移回写
            Ok(n) => n < MEMORY_SCHEMA_VERSION.parse::<i64>().unwrap_or(1),
            Err(_) => false, // 未知/不可解析版本：保守不回写
        },
    };
    if downgrade_ok {
        // 迁移表：暂无迁移（版本不符不炸）。未来破坏性 schema 变更在此追加迁移步骤。
        db.execute(
            "INSERT INTO meta(key, value) VALUES('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [MEMORY_SCHEMA_VERSION],
        )?;
    }
    Ok(())
}

/// 主表与 trigram 表行数不一致时对账回填（首次升级、崩溃失步均自愈）。
fn ensure_cjk_backfill(db: &rusqlite::Connection) -> Result<()> {
    let main_count: i64 = db.query_row("SELECT COUNT(*) FROM memory_fts", [], |r| r.get(0))?;
    let cjk_count: i64 = db.query_row("SELECT COUNT(*) FROM memory_fts_cjk", [], |r| r.get(0))?;
    if main_count != cjk_count {
        db.execute(
            "INSERT INTO memory_fts_cjk(content, tags, category, source, created_at, importance, id)
             SELECT content, tags, category, source, created_at, importance, id FROM memory_fts
             WHERE id NOT IN (SELECT id FROM memory_fts_cjk)",
            [],
        )?;
    }
    Ok(())
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
        db.execute_batch(MEMORY_SCHEMA_SQL)?;
        ensure_schema_version(&db)?;
        ensure_cjk_backfill(&db)?;

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
        db.execute_batch(MEMORY_SCHEMA_SQL)?;
        ensure_schema_version(&db)?;
        ensure_cjk_backfill(&db)?;
        Ok(Self {
            db: Arc::new(Mutex::new(db)),
        })
    }

    /// Store a memory entry.
    pub fn store(&self, entry: &MemoryEntry) -> Result<()> {
        let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let tags_str = entry.tags.join(" ");
        // F2：主表 / trigram 表 / meta 三处写入必须原子，崩溃不产生永久失步。
        let tx = db.transaction()?;
        // Delete existing entry with same id first (upsert pattern)
        tx.execute(
            "DELETE FROM memory_fts WHERE id = ?1",
            rusqlite::params![&entry.id],
        )?;
        tx.execute(
            "DELETE FROM memory_fts_cjk WHERE id = ?1",
            rusqlite::params![&entry.id],
        )?;
        tx.execute(
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
        tx.execute(
            "INSERT INTO memory_fts_cjk (content, tags, category, source, created_at, importance, id)
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
        tx.execute(
            "INSERT INTO memory_meta (id, stage, recall_count, created_at, importance)
             VALUES (?1, 'candidate', 0, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET importance = excluded.importance",
            rusqlite::params![&entry.id, entry.created_at, entry.importance],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Search memories by full-text query. Returns ranked results.
    /// 排序融合生命周期因子，权重为 [`DEFAULT_RANK_WEIGHT`]（对齐配置默认）。
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<MemorySearchResult>> {
        self.search_with_weight(query, limit, DEFAULT_RANK_WEIGHT)
    }

    /// 带生命周期融合权重的检索。权重 = 0 时与纯 bm25 排序等价（旧行为）。
    /// archived 条目不参与召回。
    pub fn search_with_weight(
        &self,
        query: &str,
        limit: usize,
        rank_weight: f64,
    ) -> Result<Vec<MemorySearchResult>> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        run_memory_search(&db, query, None, limit, rank_weight)
    }

    /// Search within a specific category.
    pub fn search_category(
        &self,
        query: &str,
        category: MemoryCategory,
        limit: usize,
    ) -> Result<Vec<MemorySearchResult>> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        run_memory_search(&db, query, Some(category), limit, DEFAULT_RANK_WEIGHT)
    }

    /// Delete a memory by ID.
    pub fn delete(&self, id: &str) -> Result<bool> {
        let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        // F2：与 store 一致，三表删除原子化。
        let tx = db.transaction()?;
        let rows = tx.execute(
            "DELETE FROM memory_fts WHERE id = ?1",
            rusqlite::params![id],
        )?;
        tx.execute(
            "DELETE FROM memory_fts_cjk WHERE id = ?1",
            rusqlite::params![id],
        )?;
        // 同步清理 lifecycle 伴行，避免孤儿 meta 积累与状态不一致。
        tx.execute(
            "DELETE FROM memory_meta WHERE id = ?1",
            rusqlite::params![id],
        )?;
        tx.commit()?;
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

    /// 读取全部真实条目（存在于 memory_fts）的 lifecycle 元数据，供衰减/清理遍历。
    /// 缺失 meta 行的条目以 fts 列兜底（candidate / 0 次召回 / created_at / importance）。
    pub fn all_lifecycle(&self) -> Result<Vec<(String, LifecycleMeta)>> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = db.prepare(
            "SELECT f.id, COALESCE(m.stage, 'candidate'), COALESCE(m.recall_count, 0), \
             m.last_recalled_at, COALESCE(m.created_at, f.created_at), \
             COALESCE(m.importance, f.importance) \
             FROM memory_fts f LEFT JOIN memory_meta m ON m.id = f.id",
        )?;
        let rows = stmt.query_map([], |r| {
            let id: String = r.get(0)?;
            let stage: String = r.get(1)?;
            let recall_count: i64 = r.get(2)?;
            let last_recalled_at: Option<i64> = r.get(3)?;
            let created_at: i64 = r.get(4)?;
            let importance: f64 = r.get(5)?;
            Ok((
                id,
                LifecycleMeta {
                    stage: MemoryLifecycleStage::parse(&stage),
                    recall_count: recall_count as u32,
                    last_recalled_at,
                    created_at,
                    importance: importance as f32,
                },
            ))
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// 持久化一条 lifecycle 元数据（decay/晋级后写回；meta 缺失时补行）。
    pub fn update_lifecycle(&self, id: &str, meta: &LifecycleMeta) -> Result<()> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        db.execute(
            "INSERT INTO memory_meta (id, stage, recall_count, last_recalled_at, created_at, importance)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                stage = excluded.stage,
                recall_count = excluded.recall_count,
                last_recalled_at = excluded.last_recalled_at,
                importance = excluded.importance",
            rusqlite::params![
                id,
                meta.stage.as_str(),
                meta.recall_count as i64,
                meta.last_recalled_at,
                meta.created_at,
                meta.importance as f64,
            ],
        )?;
        Ok(())
    }

    /// 事务化批量衰减：在**单一 SQLite 事务**内完成读-算-写，
    /// 消除 `all_lifecycle()` + 逐条 `update_lifecycle()` 的跨锁 read-modify-write
    /// （并发 `record_recall` 的 recall_count/last_recalled_at 增量不再被覆盖写回冲掉）。
    /// `decay_rate` 在入口 clamp 到 `0.0..=1.0`（负数会使 importance 上升，>1 一次清零）。
    /// 返回发生衰减（importance 实际下降）的条数。permanent 豁免。
    pub fn decay_all(&self, decay_rate: f32) -> Result<usize> {
        let decay_rate = decay_rate.clamp(0.0, 1.0);
        let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let tx = db.transaction()?;
        // 事务内读快照：与 all_lifecycle 同款 LEFT JOIN（meta 缺失兜底 fts 列）。
        let mut stmt = tx.prepare(
            "SELECT f.id, COALESCE(m.stage, 'candidate'), COALESCE(m.recall_count, 0), \
             m.last_recalled_at, COALESCE(m.created_at, f.created_at), \
             COALESCE(m.importance, f.importance) \
             FROM memory_fts f LEFT JOIN memory_meta m ON m.id = f.id",
        )?;
        let rows = stmt.query_map([], |r| {
            let id: String = r.get(0)?;
            let stage: String = r.get(1)?;
            let recall_count: i64 = r.get(2)?;
            let last_recalled_at: Option<i64> = r.get(3)?;
            let created_at: i64 = r.get(4)?;
            let importance: f64 = r.get(5)?;
            Ok((
                id,
                LifecycleMeta {
                    stage: MemoryLifecycleStage::parse(&stage),
                    recall_count: recall_count as u32,
                    last_recalled_at,
                    created_at,
                    importance: importance as f32,
                },
            ))
        })?;
        let mut decayed = 0;
        for row in rows {
            let (id, mut meta) = row?;
            if meta.stage == MemoryLifecycleStage::Permanent {
                continue;
            }
            let before = meta.importance;
            meta.apply_decay(decay_rate);
            if meta.importance < before {
                tx.execute(
                    "INSERT INTO memory_meta (id, stage, recall_count, last_recalled_at, created_at, importance)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT(id) DO UPDATE SET
                        stage = excluded.stage,
                        recall_count = excluded.recall_count,
                        last_recalled_at = excluded.last_recalled_at,
                        importance = excluded.importance",
                    rusqlite::params![
                        id,
                        meta.stage.as_str(),
                        meta.recall_count as i64,
                        meta.last_recalled_at,
                        meta.created_at,
                        meta.importance as f64,
                    ],
                )?;
                decayed += 1;
            }
        }
        drop(stmt); // 释放对 tx 的借用后再 commit
        tx.commit()?;
        Ok(decayed)
    }

    /// 删除 archived 且最后召回（无召回按创建时间）早于 cutoff 的记忆；
    /// 三表原子删除。返回删除条数。
    pub fn delete_archived_older_than(&self, cutoff_ts: i64) -> Result<usize> {
        let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let tx = db.transaction()?;
        let ids: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT f.id FROM memory_fts f
                 JOIN memory_meta m ON m.id = f.id
                 WHERE m.stage = 'archived'
                   AND COALESCE(m.last_recalled_at, m.created_at) < ?1",
            )?;
            let rows = stmt.query_map([cutoff_ts], |r| r.get::<_, String>(0))?;
            rows.filter_map(|r| r.ok()).collect()
        };
        let mut deleted = 0;
        for id in &ids {
            deleted += tx.execute("DELETE FROM memory_fts WHERE id = ?1", [id])?;
            tx.execute("DELETE FROM memory_fts_cjk WHERE id = ?1", [id])?;
            tx.execute("DELETE FROM memory_meta WHERE id = ?1", [id])?;
        }
        tx.commit()?;
        Ok(deleted)
    }

    /// 各 stage 的真实条目分布（archived 也计入；按 stage 名排序）。
    pub fn stage_counts(&self) -> Result<Vec<(String, usize)>> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = db.prepare(
            "SELECT COALESCE(m.stage, 'candidate') AS stage, COUNT(*) AS n
             FROM memory_fts f LEFT JOIN memory_meta m ON m.id = f.id
             GROUP BY stage ORDER BY stage",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize))
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// 写入（或覆盖）某条记忆的嵌入向量。meta 行不存在时先补占位行。
    pub fn upsert_embedding(&self, id: &str, vec: &[f32], model: &str) -> Result<()> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let mut blob = Vec::with_capacity(vec.len() * 4);
        for v in vec {
            blob.extend_from_slice(&v.to_le_bytes());
        }
        let updated = db.execute(
            "UPDATE memory_meta SET embedding = ?1, embed_dim = ?2, embed_model = ?3
             WHERE id = ?4",
            rusqlite::params![blob, vec.len() as i64, model, id],
        )?;
        if updated == 0 {
            db.execute(
                "INSERT INTO memory_meta
                    (id, stage, recall_count, created_at, importance, embedding, embed_dim, embed_model)
                 VALUES (?1, 'candidate', 0, ?2, 0.5, ?3, ?4, ?5)",
                rusqlite::params![id, Utc::now().timestamp(), blob, vec.len() as i64, model],
            )?;
        }
        Ok(())
    }

    /// 读取嵌入向量 + 模型名；无嵌入或 meta 缺失 → None。
    pub fn get_embedding(&self, id: &str) -> Result<Option<(Vec<f32>, String)>> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        match db.query_row(
            "SELECT embedding, embed_dim, embed_model FROM memory_meta
             WHERE id = ?1 AND embedding IS NOT NULL",
            rusqlite::params![id],
            |r| {
                let blob: Vec<u8> = r.get(0)?;
                let dim: i64 = r.get(1)?;
                let model: String = r.get(2)?;
                let mut vec = Vec::with_capacity(dim as usize);
                for chunk in blob.chunks_exact(4) {
                    vec.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                }
                Ok((vec, model))
            },
        ) {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// 混合检索：FTS（含中文路由）∪ 嵌入余弦，`0.5*bm25归一化 + 0.5*cosine`
    /// 融合排序。provider 为 None、查询嵌入失败或库中无同模型嵌入时，
    /// 行为与 [`Self::search`] 完全一致（纯 FTS）。
    pub fn search_hybrid(
        &self,
        query: &str,
        limit: usize,
        provider: Option<&dyn EmbeddingProvider>,
        model: &str,
    ) -> Result<Vec<MemorySearchResult>> {
        let fts = self.search(query, limit.saturating_mul(2))?;
        let Some(p) = provider else {
            return Ok(fts.into_iter().take(limit).collect());
        };
        let qv = match p.embed(query) {
            Ok(v) => v,
            Err(_) => return Ok(fts.into_iter().take(limit).collect()),
        };
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());

        // 1. 扫描同模型嵌入，算余弦。
        let mut emb_hits: Vec<(String, f32)> = Vec::new();
        {
            let mut stmt = db.prepare(
                "SELECT id, embedding, embed_dim, embed_model FROM memory_meta
                 WHERE embedding IS NOT NULL AND stage != 'archived'",
            )?;
            let rows = stmt.query_map([], |r| {
                let id: String = r.get(0)?;
                let blob: Vec<u8> = r.get(1)?;
                let dim: i64 = r.get(2)?;
                let m: String = r.get(3)?;
                let mut vec = Vec::with_capacity(dim as usize);
                for chunk in blob.chunks_exact(4) {
                    vec.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                }
                Ok((id, m, vec))
            })?;
            for row in rows {
                let (id, m, vec) = row?;
                if m != model {
                    continue;
                }
                let c = crate::memory::embedding::cosine(&qv, &vec);
                if c > 0.0 {
                    emb_hits.push((id, c));
                }
            }
        }
        emb_hits.sort_by(|a, b| b.1.total_cmp(&a.1));

        // 2. FTS 归一化基数。
        let mut fts_map: HashMap<String, f64> = HashMap::new();
        let mut max_s: f64 = 0.0;
        for r in &fts {
            max_s = max_s.max(r.score);
            fts_map.insert(r.entry.id.clone(), r.score);
        }
        let max_s = if max_s > 0.0 { max_s } else { 1.0 };

        // 3. 合并 id 集：FTS 顺序在前，嵌入独有命中补尾。
        let mut ids: Vec<String> = fts.iter().map(|r| r.entry.id.clone()).collect();
        let mut seen: HashSet<String> = ids.iter().cloned().collect();
        for (id, _) in &emb_hits {
            if seen.insert(id.clone()) {
                ids.push(id.clone());
            }
        }

        // 4. 拉取条目（嵌入独有 id 也要有完整记录）。
        let mut entries: HashMap<String, MemorySearchResult> = HashMap::new();
        for r in fts {
            entries.insert(r.entry.id.clone(), r);
        }
        for chunk in ids.chunks(200) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "SELECT id, content, tags, category, source, created_at, importance, 0 AS score
                 FROM memory_fts WHERE id IN ({placeholders})"
            );
            let mut stmt = db.prepare(&sql)?;
            let refs: Vec<&str> = chunk.iter().map(|s| s.as_str()).collect();
            for r in stmt
                .query_map(rusqlite::params_from_iter(refs), memory_row)?
                .flatten()
            {
                entries.insert(r.entry.id.clone(), r);
            }
        }

        // 5. 融合打分并排序。
        let mut out: Vec<MemorySearchResult> = ids
            .iter()
            .filter_map(|id| entries.get(id).cloned())
            .collect();
        for r in &mut out {
            let s = fts_map.get(&r.entry.id).copied().unwrap_or(0.0);
            let c = emb_hits
                .iter()
                .find(|(id, _)| *id == r.entry.id)
                .map(|(_, c)| *c as f64)
                .unwrap_or(0.0);
            r.score = 0.5 * (s / max_s) + 0.5 * c.max(0.0);
        }
        out.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.entry.id.cmp(&b.entry.id))
        });
        out.truncate(limit);
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Search routing: unicode61 FTS（拉丁/代码） ↔ trigram FTS（CJK）↔ LIKE 回退
// ---------------------------------------------------------------------------

/// 分词（保留原始 token，供三种路径共用）。
fn query_tokens(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

/// 是否走 trigram 表：查询含 CJK 且至少一个 token 长度 ≥3
/// （trigram 只能匹配 3 字符以上的子串）。
fn use_cjk_table(query: &str, tokens: &[String]) -> bool {
    crate::tokens::has_cjk(query) && tokens.iter().any(|t| t.chars().count() >= 3)
}

/// 是否走 LIKE 回退：无 ≥3 字符 token（含 CJK 短词与英文短词）。
fn use_like_fallback(tokens: &[String]) -> bool {
    !tokens.iter().any(|t| t.chars().count() >= 3)
}

/// 统一行解析（各路径列序一致：id/content/tags/category/source/created_at/importance/score）。
fn memory_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemorySearchResult> {
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
    Ok(MemorySearchResult {
        snippet: entry.content.chars().take(100).collect(),
        entry,
        score: -score,
    })
}

/// 带路由的检索：FTS（unicode61 / trigram）或 LIKE 回退。
/// 一律排除 archived（LEFT JOIN memory_meta 过滤）；FTS 两路在 bm25 上融合
/// 生命周期因子：`bm25 + rank_weight * ((1-importance) * stage_mult - recency_discount)`，
/// stage_mult：permanent=1.2 / verified=1.1 / candidate=1.0；recency_discount：
/// 7 天内召回 0.5、30 天内 0.25、否则 0（与 lifecycle::apply_decay 同款语义）。
fn run_memory_search(
    db: &rusqlite::Connection,
    query: &str,
    category: Option<MemoryCategory>,
    limit: usize,
    rank_weight: f64,
) -> Result<Vec<MemorySearchResult>> {
    let tokens = query_tokens(query);
    if tokens.is_empty() {
        return Ok(Vec::new());
    }

    if use_like_fallback(&tokens) {
        // LIKE 回退：短词（含 1-2 字中文）无法走 FTS/trigram。
        // 排序沿用 importance DESC（生命周期信号），同时排除 archived。
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let mut where_clauses: Vec<String> = Vec::new();
        for (i, t) in tokens.iter().enumerate() {
            let n = i + 1;
            where_clauses.push(format!(
                "(f.content LIKE ?{n} COLLATE NOCASE OR f.tags LIKE ?{n} COLLATE NOCASE \
                     OR f.source LIKE ?{n} COLLATE NOCASE)"
            ));
            params.push(Box::new(format!("%{t}%")));
        }
        let mut sql = format!(
            "SELECT f.id, f.content, f.tags, f.category, f.source, f.created_at, \
             COALESCE(m.importance, f.importance) AS importance, \
             -COALESCE(m.importance, f.importance) AS score \
             FROM memory_fts f LEFT JOIN memory_meta m ON m.id = f.id \
             WHERE {} AND COALESCE(m.stage, 'candidate') != 'archived'",
            where_clauses.join(" OR ")
        );
        if let Some(cat) = category {
            sql.push_str(" AND f.category = ?");
            params.push(Box::new(cat.as_str().to_string()));
        }
        let limit_idx = params.len() + 1;
        sql.push_str(&format!(
            " ORDER BY importance DESC, f.created_at DESC LIMIT ?{limit_idx}"
        ));
        params.push(Box::new(limit as i64));

        let mut stmt = db.prepare(&sql)?;
        let raw_params = rusqlite::params_from_iter(params.iter().map(|p| p.as_ref()));
        let results = stmt
            .query_map(raw_params, memory_row)?
            .filter_map(|r| r.ok())
            .collect();
        return Ok(results);
    }

    let cjk_mode = use_cjk_table(query, &tokens);
    let table = if cjk_mode {
        "memory_fts_cjk"
    } else {
        "memory_fts"
    };
    // trigram 表需要 ≥3 字符 token；短 token 在 cjk 路径下丢弃。
    let fts_tokens: Vec<String> = tokens
        .iter()
        .filter(|t| !cjk_mode || t.chars().count() >= 3)
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();
    if fts_tokens.is_empty() {
        return Ok(Vec::new());
    }
    let safe_query = fts_tokens.join(" OR ");
    let category_sql = if category.is_some() {
        " AND t.category = ?2"
    } else {
        ""
    };
    let limit_idx = if category.is_some() { 3 } else { 2 };
    // 生命周期因子在 SQL 内计算（数值常量内联，无注入面）。
    let now = Utc::now().timestamp();
    let lifecycle_sql = format!(
        "(1.0 - COALESCE(m.importance, t.importance)) \
         * CASE COALESCE(m.stage, 'candidate') \
             WHEN 'permanent' THEN 1.2 \
             WHEN 'verified' THEN 1.1 \
             ELSE 1.0 END \
         - COALESCE(CASE WHEN m.last_recalled_at >= {c7} THEN 0.5 \
                         WHEN m.last_recalled_at >= {c30} THEN 0.25 \
                         ELSE 0.0 END, 0.0)",
        c7 = now - 7 * 86_400,
        c30 = now - 30 * 86_400,
    );
    let sql = format!(
        "SELECT t.id, t.content, t.tags, t.category, t.source, t.created_at, \
         COALESCE(m.importance, t.importance), \
         bm25({table}) + {w} * ({lifecycle}) AS score \
         FROM {table} t LEFT JOIN memory_meta m ON m.id = t.id \
         WHERE {table} MATCH ?1 AND COALESCE(m.stage, 'candidate') != 'archived'{category_sql} \
         ORDER BY score LIMIT ?{limit_idx}",
        w = rank_weight,
        lifecycle = lifecycle_sql,
    );
    let mut stmt = db.prepare(&sql)?;
    let results = match category {
        Some(cat) => stmt
            .query_map(
                rusqlite::params![safe_query, cat.as_str(), limit as i64],
                memory_row,
            )?
            .filter_map(|r| r.ok())
            .collect(),
        None => stmt
            .query_map(rusqlite::params![safe_query, limit as i64], memory_row)?
            .filter_map(|r| r.ok())
            .collect(),
    };
    Ok(results)
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

    #[test]
    fn search_finds_chinese_content_via_trigram() {
        let store = MemoryStore::open_in_memory().unwrap();
        store
            .store(&make_entry(
                "修复了 Windows 路径分隔符导致的 gitignore 匹配失败",
                MemoryCategory::Task,
                vec!["windows".into(), "gitignore".into()],
                "session-1",
                0.9,
            ))
            .unwrap();

        let results = store.search("路径分隔符", 10).unwrap();
        assert!(
            !results.is_empty(),
            "trigram 中文子串检索应命中（查询 4 字 ≥3）"
        );
        assert!(results[0].entry.content.contains("路径分隔符"));
    }

    #[test]
    fn search_short_chinese_falls_back_to_like() {
        let store = MemoryStore::open_in_memory().unwrap();
        store
            .store(&make_entry(
                "验证命令超时会触发有界回炉",
                MemoryCategory::Skill,
                vec![],
                "auto-distill",
                0.8,
            ))
            .unwrap();

        // 2 字中文查询：trigram 无法匹配，走 LIKE 回退。
        let results = store.search("超时", 10).unwrap();
        assert!(!results.is_empty(), "2 字中文短查询应命中");
        assert!(results[0].entry.content.contains("超时"));
    }

    #[test]
    fn search_short_english_falls_back_to_like() {
        let store = MemoryStore::open_in_memory().unwrap();
        store
            .store(&make_entry(
                "AI 编译器按 token 预算裁剪上下文",
                MemoryCategory::Task,
                vec!["ai".into()],
                "session-2",
                0.7,
            ))
            .unwrap();

        // "ai" 只有 2 字符：unicode61 FTS 大小写归一后也不稳，走 LIKE NOCASE。
        let results = store.search("ai", 10).unwrap();
        assert!(!results.is_empty(), "英文短词 LIKE 回退应命中");
    }

    #[test]
    fn search_category_routes_cjk_too() {
        let store = MemoryStore::open_in_memory().unwrap();
        store
            .store(&make_entry(
                "使用 trigram 分词器增强中文记忆检索",
                MemoryCategory::Skill,
                vec!["trigram".into()],
                "auto-distill",
                0.85,
            ))
            .unwrap();
        store
            .store(&make_entry(
                "unrelated english note",
                MemoryCategory::Task,
                vec![],
                "session-3",
                0.5,
            ))
            .unwrap();

        let results = store
            .search_category("分词器", MemoryCategory::Skill, 10)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].entry.content.contains("分词器"));
    }

    #[test]
    fn upsert_keeps_cjk_table_in_sync() {
        let store = MemoryStore::open_in_memory().unwrap();
        let mut e = make_entry("初始中文记忆内容", MemoryCategory::Task, vec![], "t", 0.5);
        store.store(&e).unwrap();
        e.content = "更新后的中文内容".into();
        store.store(&e).unwrap();

        assert_eq!(store.count().unwrap(), 1, "upsert 不重复");
        let results = store.search("更新后", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].entry.content.contains("更新后"));
        // 旧内容不得再命中
        let stale = store.search("初始中文", 10).unwrap();
        assert!(stale.is_empty(), "upsert 后旧词不应命中");
    }

    #[test]
    fn cjk_backfill_reconciles_after_desync() {
        // 模拟崩溃失步：cjk 表少一行；重新打开库时应自动对账回填。
        let path = std::env::temp_dir().join(format!(
            "dnv-mem-desync-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        {
            let store = MemoryStore::open(&path).unwrap();
            store
                .store(&make_entry(
                    "第一条中文记忆",
                    MemoryCategory::Task,
                    vec![],
                    "t",
                    0.5,
                ))
                .unwrap();
            store
                .store(&make_entry(
                    "第二条中文记忆",
                    MemoryCategory::Task,
                    vec![],
                    "t",
                    0.5,
                ))
                .unwrap();
            let db = store.db.lock().unwrap_or_else(|e| e.into_inner());
            db.execute(
                "DELETE FROM memory_fts_cjk WHERE id IN (SELECT id FROM memory_fts_cjk LIMIT 1)",
                [],
            )
            .unwrap();
        }

        let reopened = MemoryStore::open(&path).unwrap();
        let hits = reopened.search("中文", 10).unwrap();
        assert_eq!(
            hits.len(),
            2,
            "reopen must reconcile the desynced trigram table"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn open_writes_schema_version() {
        let store = MemoryStore::open_in_memory().unwrap();
        let db = store.db.lock().unwrap_or_else(|e| e.into_inner());
        let v: String = db
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, "1", "schema_version 初值必须为 1");
    }

    #[test]
    fn reopen_with_older_schema_version_does_not_crash() {
        let path = std::env::temp_dir().join(format!(
            "dnv-mem-ver-old-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        {
            let store = MemoryStore::open(&path).unwrap();
            let db = store.db.lock().unwrap_or_else(|e| e.into_inner());
            db.execute(
                "UPDATE meta SET value = '0' WHERE key = 'schema_version'",
                [],
            )
            .unwrap();
        }
        // 旧版本库打开：迁移表为空 → 不炸，回写当前版本。
        let reopened = MemoryStore::open(&path).unwrap();
        let db = reopened.db.lock().unwrap_or_else(|e| e.into_inner());
        let v: String = db
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, "1", "旧版本库打开后应回写当前版本");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reopen_with_future_schema_version_does_not_crash() {
        let path = std::env::temp_dir().join(format!(
            "dnv-mem-ver-future-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        {
            let store = MemoryStore::open(&path).unwrap();
            let db = store.db.lock().unwrap_or_else(|e| e.into_inner());
            db.execute(
                "UPDATE meta SET value = '999' WHERE key = 'schema_version'",
                [],
            )
            .unwrap();
        }
        // 未知未来版本库：不炸（迁移表为空 = 无操作），且**不回写降级**版本号。
        let reopened = MemoryStore::open(&path).unwrap();
        let hits = reopened.search("anything", 10).unwrap();
        assert!(hits.is_empty(), "未来版本库打开后检索可用");
        let db = reopened.db.lock().unwrap_or_else(|e| e.into_inner());
        let v: String = db
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            v, "999",
            "未来版本库打开后必须保持原版本号（收紧：修正此前静默降级回写的 bug 行为断言）"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn search_excludes_archived_entries() {
        let store = MemoryStore::open_in_memory().unwrap();
        let keep = make_entry(
            "rust borrow checker for memory safety",
            MemoryCategory::Task,
            vec![],
            "t",
            0.5,
        );
        let archive = make_entry(
            "rust borrow legacy note superseded",
            MemoryCategory::Task,
            vec![],
            "t",
            0.5,
        );
        store.store(&keep).unwrap();
        store.store(&archive).unwrap();
        {
            let db = store.db.lock().unwrap_or_else(|e| e.into_inner());
            db.execute(
                "UPDATE memory_meta SET stage = 'archived' WHERE id = ?1",
                [&archive.id],
            )
            .unwrap();
        }
        let hits = store.search("rust borrow", 10).unwrap();
        assert_eq!(hits.len(), 1, "archived 不参与 FTS 召回");
        assert_eq!(hits[0].entry.id, keep.id);
    }

    #[test]
    fn search_like_fallback_excludes_archived() {
        let store = MemoryStore::open_in_memory().unwrap();
        let keep = make_entry(
            "ai tools for parsing config",
            MemoryCategory::Task,
            vec![],
            "t",
            0.5,
        );
        let archive = make_entry(
            "ai legacy experiment discarded",
            MemoryCategory::Task,
            vec![],
            "t",
            0.5,
        );
        store.store(&keep).unwrap();
        store.store(&archive).unwrap();
        {
            let db = store.db.lock().unwrap_or_else(|e| e.into_inner());
            db.execute(
                "UPDATE memory_meta SET stage = 'archived' WHERE id = ?1",
                [&archive.id],
            )
            .unwrap();
        }
        // "ai" 为 2 字符 → LIKE 回退路径。
        let hits = store.search("ai", 10).unwrap();
        assert_eq!(hits.len(), 1, "archived 不参与 LIKE 回退召回");
        assert_eq!(hits[0].entry.id, keep.id);
    }

    #[test]
    fn search_fuses_lifecycle_ranking() {
        let store = MemoryStore::open_in_memory().unwrap();
        // 同文本 → bm25 分完全相等；排序差异只能来自生命周期因子。
        let low = make_entry(
            "rust borrow checker",
            MemoryCategory::Task,
            vec![],
            "t",
            0.1,
        );
        let high = make_entry(
            "rust borrow checker",
            MemoryCategory::Task,
            vec![],
            "t",
            0.9,
        );
        store.store(&low).unwrap();
        store.store(&high).unwrap();
        // 把 high 提升到 permanent（模拟多次召回晋级后的状态）。
        let meta = LifecycleMeta {
            stage: MemoryLifecycleStage::Permanent,
            recall_count: 3,
            last_recalled_at: None,
            created_at: low.created_at,
            importance: 0.9,
        };
        store.update_lifecycle(&high.id, &meta).unwrap();

        // weight=0：纯 bm25，两分相等 → 与旧行为一致（插入序 low 在前）。
        let w0 = store.search_with_weight("rust borrow", 10, 0.0).unwrap();
        assert_eq!(w0.len(), 2);
        assert_eq!(w0[0].entry.id, low.id, "weight=0 必须保持纯 bm25 顺序");
        assert_eq!(
            w0[0].score, w0[1].score,
            "weight=0 时同文本两条目分数必须相等（生命周期项已关闭）"
        );

        // weight=0.3：permanent+高 importance 的 high 必须反超。
        let w03 = store
            .search_with_weight("rust borrow", 10, DEFAULT_RANK_WEIGHT)
            .unwrap();
        assert_eq!(w03[0].entry.id, high.id, "生命周期融合必须重排");
    }

    #[test]
    fn stage_counts_reports_distribution() {
        let store = MemoryStore::open_in_memory().unwrap();
        let e1 = make_entry("entry one", MemoryCategory::Task, vec![], "t", 0.5);
        let e2 = make_entry("entry two", MemoryCategory::Task, vec![], "t", 0.5);
        let e3 = make_entry("entry three", MemoryCategory::Task, vec![], "t", 0.5);
        store.store(&e1).unwrap();
        store.store(&e2).unwrap();
        store.store(&e3).unwrap();
        store.record_recall(&e2.id).unwrap(); // verified
        {
            let db = store.db.lock().unwrap_or_else(|e| e.into_inner());
            db.execute(
                "UPDATE memory_meta SET stage = 'archived' WHERE id = ?1",
                [&e3.id],
            )
            .unwrap();
        }
        let counts = store.stage_counts().unwrap();
        let get = |stage: &str| {
            counts
                .iter()
                .find(|(s, _)| s == stage)
                .map(|(_, n)| *n)
                .unwrap_or(0)
        };
        assert_eq!(get("candidate"), 1);
        assert_eq!(get("verified"), 1);
        assert_eq!(get("archived"), 1);
        assert_eq!(get("permanent"), 0);
    }
}
