# Closed-Loop Memory Engine — P1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把"两套断连的记忆系统"合一为一个持久化引擎，接线到每一次 run（起点召回注入 + 结束自动捕获经验），并给用户可审查入口——修复"agent 不会越用越聪明"的根因。

**Architecture:** 新增 `MemoryEngine`（core，薄门面，包 `Arc<MemoryStore>`），在 FTS5 之外持久化 lifecycle（`memory_meta` 表）与成本/召回计数（`counters`/`distill_log` 表）。`remember/recall/forget` 工具改用经 `ToolContext.extensions` 注入的 `MemoryHandle`（照搬 `GraphHandle` 先例），删除易失 static HashMap。Agent 新增起点召回注入（volatile 区，不破前缀缓存）与结束沉淀钩子；runtime 统一装配；CLI 提供 `memory list/search/forget/stats`。

**Tech Stack:** Rust、rusqlite + FTS5(BM25)、tokio、async-trait、regex、clap、thiserror。

---

## P1 范围与显式取舍（对齐已评审 spec 的分期）

**做（P1）：** 统一持久化记忆、lifecycle 持久化、redaction 脱敏、护栏 + 每日/每会话成本上限、起点召回注入、结束**启发式**经验捕获（任务摘要 + 失败教训，确定性、零额外 token）、CLI 审查入口、可观测性计数。

**不做（延后到 P2/P3，spec 已记录）：**
- `Embedder` trait 与向量/语义检索、混合 RRF（P2）。
- **LLM 合成技能/画像**沉淀（P2）——P1 只做确定性捕获，先用 §可观测性数据验证"简单记忆是否会被复用"，再决定是否花 token 做合成。
- 分层归纳 consolidation（P3）。

> 该取舍把 LLM 调用完全移出 P1，使 P1 全链路可确定性测试、零额外成本，且符合"先跑数据再加码"的立场。若你（用户）希望 P1 就带 LLM 合成，请在执行前说明。

---

## 文件结构（P1 触及）

| 文件 | 动作 | 职责 |
|---|---|---|
| `crates/deepseeknova-config/src/lib.rs` | 改 | 新增 `MemoryConfig` + `Config.memory` 字段 + merge + 测试 |
| `crates/deepseeknova-core/src/memory/redact.rs` | 建 | `redact(&str)->String` 密钥脱敏 |
| `crates/deepseeknova-core/src/memory/store.rs` | 改 | `memory_meta`/`counters`/`distill_log` schema + WAL + lifecycle/计数方法 |
| `crates/deepseeknova-core/src/memory/engine.rs` | 建 | `MemoryEngine` 门面（recall/remember/forget/record_task/stats） |
| `crates/deepseeknova-core/src/memory/mod.rs` | 改 | 导出 `redact`、`engine` |
| `crates/deepseeknova-core/Cargo.toml` | 改 | 加 `regex` 依赖 |
| `crates/deepseeknova-tools/src/memory.rs` | 改（重写） | `remember/recall/forget` 改接 `MemoryHandle`，删除 static HashMap |
| `crates/deepseeknova-agent/src/agent.rs` | 改 | `with_recall_provider` / `with_distill_hook` + 注入/钩子逻辑 |
| `crates/deepseeknova-runtime/src/lib.rs` | 改 | 构造 `MemoryEngine`、注入 handle、装配 recall/distill |
| `crates/deepseeknova-cli/src/cli.rs` | 改 | `Memory` 子命令 + `MemoryAction` |
| `crates/deepseeknova-cli/src/main.rs` | 改 | dispatch `memory` 子命令 |

约定：`MemoryHandle = std::sync::Arc<deepseeknova_core::memory::engine::MemoryEngine>`（定义在 tools/memory.rs，与 `GraphHandle` 对称）。

---

## Task 1: `[memory]` 配置节

**Files:**
- Modify: `crates/deepseeknova-config/src/lib.rs`（新增 `MemoryConfig`，挂到 `Config`，测试）

- [ ] **Step 1: 写失败测试**

在 `crates/deepseeknova-config/src/lib.rs` 的 `mod tests` 内追加：

```rust
    #[test]
    fn memory_config_defaults() {
        let c = Config::default();
        assert!(c.memory.enabled);
        assert!(c.memory.auto_learn);
        assert!(c.memory.redact_secrets);
        assert_eq!(c.memory.embedder, "none");
        assert_eq!(c.memory.recall_inject_tokens, 200);
        assert_eq!(c.memory.recall_top_k, 3);
        assert_eq!(c.memory.min_tool_calls, 5);
        assert_eq!(c.memory.min_steps, 3);
        assert_eq!(c.memory.max_distillations_per_day, 50);
        assert_eq!(c.memory.max_distillations_per_session, 10);
        assert_eq!(c.memory.db_path, ".deepseeknova/memory.db");
    }

    #[test]
    fn memory_config_parses_from_toml() {
        let toml = "[memory]\nenabled = false\nauto_learn = false\nrecall_top_k = 7\n";
        let c: Config = toml::from_str(toml).unwrap();
        assert!(!c.memory.enabled);
        assert!(!c.memory.auto_learn);
        assert_eq!(c.memory.recall_top_k, 7);
        // 未覆盖字段仍取默认
        assert!(c.memory.redact_secrets);
        assert_eq!(c.memory.recall_inject_tokens, 200);
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p deepseeknova-config memory_config -- --nocapture`
Expected: 编译失败，`no field 'memory' on type 'Config'`。

- [ ] **Step 3: 加 `MemoryConfig` 并挂到 `Config`**

在 `Config` 结构体里、`pub graph: GraphConfig` 字段之后插入：

```rust
    /// 记忆引擎配置（闭环学习）。
    #[serde(default)]
    pub memory: MemoryConfig,
```

在 `// Graph (code index)` 区块之后（`impl Default for GraphConfig` 之下）新增整节：

```rust
// ---------------------------------------------------------------------------
// Memory (closed-loop learning)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// 主开关。false = 零开销，行为等同现状。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// SQLite 记忆库路径（相对工作区根）。
    #[serde(default = "default_memory_db_path")]
    pub db_path: String,
    /// 全自动沉淀开关（依赖 redact_secrets + CLI 审查入口作为前置条件）。
    #[serde(default = "default_true")]
    pub auto_learn: bool,
    /// 写入前脱敏（auto_learn 的硬前提）。
    #[serde(default = "default_true")]
    pub redact_secrets: bool,
    /// 嵌入后端：none | local | remote（P1 恒为 none）。
    #[serde(default = "default_embedder")]
    pub embedder: String,
    /// 嵌入模型名（P2 起用）。
    #[serde(default)]
    pub embed_model: String,
    /// 起点召回注入块的 token 上限。0 = 不注入，仅保留按需工具。
    #[serde(default = "default_recall_inject_tokens")]
    pub recall_inject_tokens: usize,
    /// 起点召回条数。
    #[serde(default = "default_recall_top_k")]
    pub recall_top_k: usize,
    /// 触发沉淀的最小工具调用数。
    #[serde(default = "default_min_tool_calls")]
    pub min_tool_calls: usize,
    /// 触发沉淀的最小步数。
    #[serde(default = "default_min_steps")]
    pub min_steps: usize,
    /// 每日沉淀硬上限。
    #[serde(default = "default_max_distill_day")]
    pub max_distillations_per_day: u32,
    /// 每会话沉淀硬上限。
    #[serde(default = "default_max_distill_session")]
    pub max_distillations_per_session: u32,
}

fn default_memory_db_path() -> String {
    ".deepseeknova/memory.db".to_string()
}
fn default_embedder() -> String {
    "none".to_string()
}
fn default_recall_inject_tokens() -> usize {
    200
}
fn default_recall_top_k() -> usize {
    3
}
fn default_min_tool_calls() -> usize {
    5
}
fn default_min_steps() -> usize {
    3
}
fn default_max_distill_day() -> u32 {
    50
}
fn default_max_distill_session() -> u32 {
    10
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            db_path: default_memory_db_path(),
            auto_learn: true,
            redact_secrets: true,
            embedder: default_embedder(),
            embed_model: String::new(),
            recall_inject_tokens: 200,
            recall_top_k: 3,
            min_tool_calls: 5,
            min_steps: 3,
            max_distillations_per_day: 50,
            max_distillations_per_session: 10,
        }
    }
}
```

> `MemoryConfig` 采用整体默认（`#[serde(default)]` 在字段上），`Config::merge` 无需为它加分支——未在 project 层出现时保持 user/default 值即可；如需 project 覆盖 user，可后续补 merge，本 P1 不需要。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p deepseeknova-config memory_config -- --nocapture`
Expected: PASS（2 个测试）。

- [ ] **Step 5: 提交**

```bash
git add crates/deepseeknova-config/src/lib.rs
git commit -m "feat(config): add [memory] section for closed-loop learning"
```

---

## Task 2: 密钥脱敏模块 `redact`

**Files:**
- Create: `crates/deepseeknova-core/src/memory/redact.rs`
- Modify: `crates/deepseeknova-core/src/memory/mod.rs`（加 `pub mod redact;`）
- Modify: `crates/deepseeknova-core/Cargo.toml`（加 `regex`）

- [ ] **Step 1: 加依赖**

在 `crates/deepseeknova-core/Cargo.toml` 的 `[dependencies]` 增加（若已存在则跳过）：

```toml
regex = { workspace = true }
```

- [ ] **Step 2: 写失败测试（先建文件含测试）**

Create `crates/deepseeknova-core/src/memory/redact.rs`：

```rust
//! # Secret Redaction
//!
//! 在把任何内容写入持久记忆库前调用，抹除常见密钥/token/私钥，
//! 避免 `.env`、报错信息里的凭据被无确认写入。宁可误伤，不可泄露。

use regex::Regex;
use std::sync::OnceLock;

/// 脱敏占位符。
const MASK: &str = "[REDACTED]";

struct Patterns {
    kv: Regex,
    aws: Regex,
    pem: Regex,
    bearer: Regex,
}

// 静态常量正则：唯一可能的失败是编译期笔误，用 expect 立即暴露；
// core 禁用 unwrap/expect，故在此函数局部放行。
#[allow(clippy::expect_used)]
fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        // key/secret/token/password = <值> 或 : <值>
        kv: Regex::new(
            r#"(?i)\b(api[_-]?key|secret|token|password|passwd|access[_-]?key)\b\s*[:=]\s*['"]?[A-Za-z0-9._\-/+]{8,}['"]?"#,
        )
        .expect("kv regex"),
        // AWS Access Key ID
        aws: Regex::new(r"\bAKIA[0-9A-Z]{16}\b").expect("aws regex"),
        // PEM 私钥块头
        pem: Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----").expect("pem regex"),
        // Authorization: Bearer <token>
        bearer: Regex::new(r"(?i)bearer\s+[A-Za-z0-9._\-]{12,}").expect("bearer regex"),
    })
}

/// 返回脱敏后的字符串。无命中时返回等价内容（可能是原串的 Cow::Owned）。
pub fn redact(input: &str) -> String {
    let p = patterns();
    let s = p.kv.replace_all(input, |c: &regex::Captures| {
        format!("{}=[REDACTED]", &c[1])
    });
    let s = p.aws.replace_all(&s, MASK);
    let s = p.pem.replace_all(&s, MASK);
    let s = p.bearer.replace_all(&s, "Bearer [REDACTED]");
    s.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_api_key_kv() {
        let out = redact("export API_KEY=sk-ABCD1234efgh5678");
        assert!(!out.contains("sk-ABCD1234efgh5678"), "got: {out}");
        assert!(out.contains("[REDACTED]"));
    }

    #[test]
    fn masks_aws_and_pem_and_bearer() {
        assert!(redact("id AKIAIOSFODNN7EXAMPLE here").contains("[REDACTED]"));
        assert!(redact("-----BEGIN RSA PRIVATE KEY-----").contains("[REDACTED]"));
        assert!(redact("Authorization: Bearer abcdefghijklmnop").contains("[REDACTED]"));
    }

    #[test]
    fn leaves_ordinary_text_untouched() {
        let text = "fn main() { println!(\"hello world\"); }";
        assert_eq!(redact(text), text);
    }

    #[test]
    fn leaves_short_values_untouched() {
        // 太短的赋值不视为密钥，避免误伤（如 x = 1）
        let text = "let x = 1;";
        assert_eq!(redact(text), text);
    }
}
```

- [ ] **Step 3: 导出模块**

在 `crates/deepseeknova-core/src/memory/mod.rs` 追加一行（保持字母序）：

```rust
pub mod redact;
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p deepseeknova-core memory::redact -- --nocapture`
Expected: PASS（4 个测试）。

- [ ] **Step 5: 提交**

```bash
git add crates/deepseeknova-core/src/memory/redact.rs crates/deepseeknova-core/src/memory/mod.rs crates/deepseeknova-core/Cargo.toml
git commit -m "feat(core/memory): secret redaction before persistence"
```

---

## Task 3: 记忆库 schema 扩展（lifecycle 持久化 + 计数 + WAL）

**Files:**
- Modify: `crates/deepseeknova-core/src/memory/store.rs`

目标：在现有 FTS5 之外新增 `memory_meta`（持久化 lifecycle）、`counters`（召回命中率）、`distill_log`（每日成本上限）三张常规表；开启 WAL + busy_timeout；`store()` 同事务写 meta 行。

- [ ] **Step 1: 写失败测试**

在 `store.rs` 的 `mod tests` 内追加：

```rust
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
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p deepseeknova-core memory::store -- --nocapture`
Expected: 编译失败，`no method named 'meta'/'record_recall'/...`。

- [ ] **Step 3: 扩展 schema 与方法**

在 `store.rs` 顶部 `use` 区补充：

```rust
use crate::memory::lifecycle::{LifecycleMeta, MemoryLifecycleStage};
use std::time::Duration;
```

新增伴表元数据结构（放在 `MemorySearchResult` 定义之后）：

```rust
/// 持久化的 lifecycle 元数据行（伴随 memory_fts.id）。
#[derive(Debug, Clone)]
pub struct MetaRow {
    pub stage: String,
    pub recall_count: u32,
    pub last_recalled_at: Option<i64>,
    pub embed_dim: Option<i64>,
    pub embed_model: Option<String>,
}
```

把两处建表 SQL（`open` 与 `open_in_memory` 里的 `execute_batch`）替换为下面的**完整批处理**（在原 `memory_fts` 之后追加三张表）：

```rust
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
```

在**仅 `open`（磁盘库）**里，`execute_batch` 之前开启 WAL + busy_timeout（`open_in_memory` 不需要）：

```rust
        db.busy_timeout(Duration::from_secs(5))?;
        let _ = db.pragma_update(None, "journal_mode", "WAL");
```

修改 `store()`：在现有 `INSERT INTO memory_fts ...` 之后、返回前，追加 upsert meta 行（同一把锁内，天然事务化）：

```rust
        db.execute(
            "INSERT INTO memory_meta (id, stage, recall_count, created_at, importance)
             VALUES (?1, 'candidate', 0, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET importance = excluded.importance",
            rusqlite::params![&entry.id, entry.created_at, entry.importance],
        )?;
```

在 `impl MemoryStore` 末尾（`count` 之后）新增方法：

```rust
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
            Err(_) => return Ok(()), // 无 meta 行则跳过
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

    /// 记一次召回调用（用于命中率统计）。
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
```

> `record_recall` 复用 `lifecycle.rs` 的 `LifecycleMeta::record_recall`（Candidate→Verified 规则），把结果落回 `memory_meta`。这样"越召回越晋级"跨进程持久。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p deepseeknova-core memory::store -- --nocapture`
Expected: PASS（原有 3 个 + 新增 4 个）。

- [ ] **Step 5: 提交**

```bash
git add crates/deepseeknova-core/src/memory/store.rs
git commit -m "feat(core/memory): persist lifecycle + recall/distill counters + WAL"
```


---

## Task 4: `MemoryEngine` 门面

**Files:**
- Create: `crates/deepseeknova-core/src/memory/engine.rs`
- Modify: `crates/deepseeknova-core/src/memory/mod.rs`（加 `pub mod engine;`）

薄门面：包 `Arc<MemoryStore>`，对外提供运行路径与 CLI 需要的 API。P1 无向量、无 LLM。去重用**内容哈希 id**（相同内容 upsert，不产生重复）。

- [ ] **Step 1: 导出模块**

在 `crates/deepseeknova-core/src/memory/mod.rs` 追加（保持字母序，置于 `pub mod evidence;` 之后）：

```rust
pub mod engine;
```

- [ ] **Step 2: 写引擎 + 失败测试（先建含测试的文件）**

Create `crates/deepseeknova-core/src/memory/engine.rs`：

```rust
//! # MemoryEngine — 统一记忆门面（P1）
//!
//! 把持久 `MemoryStore`（FTS5 + lifecycle）包成运行路径与 CLI 共用的门面：
//! - `recall`：检索并记命中率 + 逐条晋级；
//! - `remember`/`forget`：显式记忆工具后端（id=key 确定性 upsert）；
//! - `record_task`：结束时启发式经验捕获（护栏 + 成本上限 + 脱敏 + 去重）；
//! - `stats`：可观测性快照，驱动 P2 决策。

use crate::memory::redact::redact;
use crate::memory::skill::{TaskObservation, TaskOutcome};
use crate::memory::store::{
    make_entry, MemoryCategory, MemoryEntry, MemorySearchResult, MemoryStore,
};
use anyhow::Result;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tracing::{info, warn};

/// 沉淀护栏（由 runtime 从 `[memory]` 配置装填）。
#[derive(Debug, Clone)]
pub struct DistillGuards {
    pub auto_learn: bool,
    pub min_tool_calls: usize,
    pub min_steps: usize,
    pub max_per_day: u32,
    pub max_per_session: u32,
}

/// 统计快照（CLI `memory stats`；P2 决策依据）。
#[derive(Debug, Clone)]
pub struct MemoryStats {
    pub total: usize,
    pub recall_hit_rate: f64,
    pub reinforce_ratio: f64,
}

/// 统一记忆引擎（P1：FTS5 + lifecycle 持久化）。
pub struct MemoryEngine {
    store: Arc<MemoryStore>,
    redact_secrets: bool,
    session_distills: AtomicU32,
}

/// 由内容派生稳定 id，实现"相同内容不重复插入"的确定性去重。
fn content_id(prefix: &str, content: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut h);
    format!("{prefix}-{:016x}", h.finish())
}

impl MemoryEngine {
    /// 打开磁盘记忆库。
    pub fn open(path: impl AsRef<Path>, redact_secrets: bool) -> Result<Self> {
        Ok(Self {
            store: Arc::new(MemoryStore::open(path.as_ref())?),
            redact_secrets,
            session_distills: AtomicU32::new(0),
        })
    }

    /// 内存库（测试 / runtime fallback 用；非 cfg(test)，故跨 crate 可用）。
    pub fn open_in_memory(redact_secrets: bool) -> Result<Self> {
        Ok(Self {
            store: Arc::new(MemoryStore::open_in_memory()?),
            redact_secrets,
            session_distills: AtomicU32::new(0),
        })
    }

    /// 召回：记命中率 + 对每条命中执行 record_recall（晋级）。
    pub fn recall(&self, query: &str, top_k: usize) -> Result<Vec<MemorySearchResult>> {
        let results = self.store.search(query, top_k)?;
        self.store.note_recall(!results.is_empty()).ok();
        for r in &results {
            self.store.record_recall(&r.entry.id).ok();
        }
        Ok(results)
    }

    /// 显式记住（id=key 确定性 upsert）。返回 key 是否已存在。
    pub fn remember(&self, key: &str, value: &str, tags: Vec<String>) -> Result<bool> {
        let existed = self.store.meta(key)?.is_some();
        let content = if self.redact_secrets {
            redact(value)
        } else {
            value.to_string()
        };
        let mut entry = make_entry(content, MemoryCategory::Task, tags, "remember-tool", 0.6);
        entry.id = key.to_string();
        self.store.store(&entry)?;
        Ok(existed)
    }

    /// 删除一条记忆。
    pub fn forget(&self, key: &str) -> Result<bool> {
        self.store.delete(key)
    }

    /// 结束时启发式经验捕获（P1，无 LLM）。返回是否实际写入。
    pub fn record_task(&self, obs: &TaskObservation, g: &DistillGuards) -> Result<bool> {
        if !g.auto_learn {
            return Ok(false);
        }
        if obs.tool_calls.len() < g.min_tool_calls || obs.steps_taken.len() < g.min_steps {
            return Ok(false);
        }
        if self.store.distill_count_today()? >= g.max_per_day {
            warn!("distill skipped: daily cap reached");
            return Ok(false);
        }
        if self.session_distills.load(Ordering::Relaxed) >= g.max_per_session {
            warn!("distill skipped: session cap reached");
            return Ok(false);
        }

        let redact_one = |s: &str| -> String {
            if self.redact_secrets {
                redact(s)
            } else {
                s.to_string()
            }
        };

        // 1) 任务摘要（Task 记忆），内容哈希 id 去重
        let summary = redact_one(&format!(
            "Task: {}\nTools used: {}\nOutcome: {:?}",
            obs.task_description,
            obs.tool_calls.join(", "),
            obs.outcome
        ));
        let mut e = make_entry(
            summary.clone(),
            MemoryCategory::Task,
            vec!["task".into(), "summary".into()],
            "auto-distill",
            0.6,
        );
        e.id = content_id("task", &summary);
        self.store.store(&e)?;

        // 2) 失败教训（Skill 记忆，tags=[failure,lesson]）——"自我总结错误形成经验"
        if obs.outcome == TaskOutcome::Failure {
            if let Some(detail) = &obs.user_feedback {
                let lesson = redact_one(&format!(
                    "[lesson] task '{}' failed: {}",
                    obs.task_description, detail
                ));
                let mut le = make_entry(
                    lesson.clone(),
                    MemoryCategory::Skill,
                    vec!["failure".into(), "lesson".into()],
                    "auto-distill",
                    0.8,
                );
                le.id = content_id("lesson", &lesson);
                self.store.store(&le)?;
            }
        }

        self.store.bump_distill_count()?;
        self.session_distills.fetch_add(1, Ordering::Relaxed);
        info!(outcome = ?obs.outcome, "task experience captured");
        Ok(true)
    }

    /// 可观测性快照。
    pub fn stats(&self) -> Result<MemoryStats> {
        let (calls, nonempty) = self.store.recall_counters()?;
        Ok(MemoryStats {
            total: self.store.count()?,
            recall_hit_rate: if calls == 0 {
                0.0
            } else {
                nonempty as f64 / calls as f64
            },
            reinforce_ratio: self.store.reinforce_ratio()?,
        })
    }

    /// 列出某类记忆（CLI）。
    pub fn list(&self, category: MemoryCategory) -> Result<Vec<MemoryEntry>> {
        self.store.list_category(category)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(tools: usize, steps: usize, outcome: TaskOutcome, feedback: Option<&str>) -> TaskObservation {
        TaskObservation {
            task_description: "build a web server".into(),
            tool_calls: vec!["write_file".into(); tools],
            steps_taken: vec!["s".into(); steps],
            outcome,
            user_feedback: feedback.map(|s| s.to_string()),
            session_id: "sess".into(),
        }
    }

    fn guards() -> DistillGuards {
        DistillGuards {
            auto_learn: true,
            min_tool_calls: 5,
            min_steps: 3,
            max_per_day: 50,
            max_per_session: 10,
        }
    }

    #[test]
    fn remember_then_recall_finds_it() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        assert!(!eng.remember("greeting", "hello from rust", vec![]).unwrap());
        let hits = eng.recall("rust", 5).unwrap();
        assert!(hits.iter().any(|h| h.entry.id == "greeting"));
        // 二次 remember 同 key = 已存在
        assert!(eng.remember("greeting", "updated", vec![]).unwrap());
    }

    #[test]
    fn remember_redacts_secrets() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        eng.remember("cred", "API_KEY=sk-ABCD1234efgh5678", vec![]).unwrap();
        let hits = eng.recall("API_KEY", 5).unwrap();
        assert!(hits.iter().all(|h| !h.entry.content.contains("sk-ABCD1234efgh5678")));
    }

    #[test]
    fn record_task_gated_below_threshold() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        // 工具/步数不够 → 不写
        assert!(!eng.record_task(&obs(2, 1, TaskOutcome::Success, None), &guards()).unwrap());
    }

    #[test]
    fn record_task_writes_summary_when_eligible() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        assert!(eng.record_task(&obs(6, 4, TaskOutcome::Success, None), &guards()).unwrap());
        let hits = eng.recall("web server", 5).unwrap();
        assert!(hits.iter().any(|h| h.entry.source == "auto-distill"));
    }

    #[test]
    fn record_task_writes_failure_lesson() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        eng.record_task(&obs(6, 4, TaskOutcome::Failure, Some("compile error E0433")), &guards()).unwrap();
        let hits = eng.recall("lesson failed", 10).unwrap();
        assert!(hits.iter().any(|h| h.entry.tags.contains(&"lesson".to_string())));
    }

    #[test]
    fn record_task_dedups_identical_summary() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        let o = obs(6, 4, TaskOutcome::Success, None);
        eng.record_task(&o, &guards()).unwrap();
        eng.record_task(&o, &guards()).unwrap();
        // 相同摘要内容 → 同 id → upsert，不重复
        let count = eng.list(MemoryCategory::Task).unwrap().len();
        assert_eq!(count, 1, "identical summary must not duplicate");
    }

    #[test]
    fn stats_reports_hit_rate() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        eng.remember("k", "rust systems programming", vec![]).unwrap();
        eng.recall("rust", 5).unwrap(); // nonempty
        eng.recall("zzz-nomatch", 5).unwrap(); // empty
        let s = eng.stats().unwrap();
        assert!((s.recall_hit_rate - 0.5).abs() < 1e-9, "got {}", s.recall_hit_rate);
    }
}
```

- [ ] **Step 3: 运行测试确认通过**

Run: `cargo test -p deepseeknova-core memory::engine -- --nocapture`
Expected: PASS（7 个测试）。

- [ ] **Step 4: 提交**

```bash
git add crates/deepseeknova-core/src/memory/engine.rs crates/deepseeknova-core/src/memory/mod.rs
git commit -m "feat(core/memory): MemoryEngine facade (recall/remember/record_task/stats)"
```

---

## Task 5: 记忆工具改接持久引擎（核心 bug 修复）

**Files:**
- Modify（整文件重写）: `crates/deepseeknova-tools/src/memory.rs`

删除易失 `static STORE` HashMap 与私有 BM25；`remember/recall/forget` 改用经 `ctx.extensions` 注入的 `MemoryHandle`，缺失时优雅降级。

- [ ] **Step 1: 用新内容整体替换 `memory.rs`**

把 `crates/deepseeknova-tools/src/memory.rs` 全文替换为：

```rust
//! 记忆工具：remember / recall / forget。
//! 持久引擎句柄经 `ToolContext.extensions` 注入（`MemoryHandle`），缺失时优雅降级。
//! 相比旧实现，写入落到跨会话持久的 SQLite 引擎，而非进程内易失 HashMap。

use async_trait::async_trait;
use deepseeknova_core::memory::engine::MemoryEngine;
use deepseeknova_core::{Tool, ToolContext, ToolSchema};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

/// 共享持久记忆引擎句柄（runtime 注入，对称于 GraphHandle）。
pub type MemoryHandle = Arc<MemoryEngine>;

/// 引擎未装配时的降级提示（不打断 run）。
const NO_MEMORY_MSG: &str = "记忆引擎未启用（[memory] enabled=false 或未装配），无法读写记忆。";

fn handle(ctx: &ToolContext) -> Option<MemoryHandle> {
    ctx.extensions.get::<MemoryHandle>().cloned()
}

// ---------------------------------------------------------------------------
// RememberTool
// ---------------------------------------------------------------------------

pub struct RememberTool;

#[derive(Deserialize)]
struct RememberArgs {
    key: String,
    value: String,
    #[serde(default)]
    tags: Vec<String>,
}

#[async_trait]
impl Tool for RememberTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "remember".to_string(),
            description: "持久记住一条信息（跨会话/重启保留），带唯一 key 与可选 tags。相同 key 覆盖更新。".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "key": {"type": "string", "description": "Unique identifier for this memory."},
                    "value": {"type": "string", "description": "Content to store."},
                    "tags": {"type": "array", "items": {"type": "string"}, "description": "Optional tags."}
                },
                "required": ["key", "value"]
            }),
        }
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        if ctx.cancellation.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let parsed: RememberArgs = serde_json::from_str(args)?;
        let h = match handle(ctx) {
            Some(h) => h,
            None => return Ok(NO_MEMORY_MSG.to_string()),
        };
        let existed = h.remember(&parsed.key, &parsed.value, parsed.tags)?;
        Ok(if existed {
            format!("updated memory '{}'", parsed.key)
        } else {
            format!("stored memory '{}'", parsed.key)
        })
    }
}

// ---------------------------------------------------------------------------
// ForgetTool
// ---------------------------------------------------------------------------

pub struct ForgetTool;

#[derive(Deserialize)]
struct ForgetArgs {
    key: String,
}

#[async_trait]
impl Tool for ForgetTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "forget".to_string(),
            description: "按 key 删除一条持久记忆。".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {"key": {"type": "string", "description": "Key to remove."}},
                "required": ["key"]
            }),
        }
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        if ctx.cancellation.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let parsed: ForgetArgs = serde_json::from_str(args)?;
        let h = match handle(ctx) {
            Some(h) => h,
            None => return Ok(NO_MEMORY_MSG.to_string()),
        };
        if h.forget(&parsed.key)? {
            Ok(format!("removed memory '{}'", parsed.key))
        } else {
            Ok(format!("memory '{}' not found", parsed.key))
        }
    }
}

// ---------------------------------------------------------------------------
// RecallTool
// ---------------------------------------------------------------------------

pub struct RecallTool;

#[derive(Deserialize)]
struct RecallArgs {
    query: String,
    #[serde(default = "default_top_k")]
    top_k: usize,
}

const fn default_top_k() -> usize {
    10
}

#[async_trait]
impl Tool for RecallTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "recall".to_string(),
            description: "在持久记忆库中按相关度检索（跨会话），返回最匹配的条目。".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Search query."},
                    "top_k": {"type": "integer", "description": "Max results (default 10).", "default": 10}
                },
                "required": ["query"]
            }),
        }
    }

    fn read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        if ctx.cancellation.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let parsed: RecallArgs = serde_json::from_str(args)?;
        let h = match handle(ctx) {
            Some(h) => h,
            None => return Ok(NO_MEMORY_MSG.to_string()),
        };
        let results = h.recall(&parsed.query, parsed.top_k)?;
        if results.is_empty() {
            return Ok(format!("no matches for '{}'", parsed.query));
        }
        let mut out = format!("found {} match(es) for '{}':\n", results.len(), parsed.query);
        for (i, r) in results.iter().enumerate() {
            let preview: String = r.entry.content.chars().take(200).collect();
            out.push_str(&format!("  {}. [{}] {}\n", i + 1, r.entry.id, preview));
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with_engine() -> (ToolContext, MemoryHandle) {
        let engine: MemoryHandle = Arc::new(MemoryEngine::open_in_memory(true).unwrap());
        let ctx = ToolContext::new("t").with_extension(engine.clone());
        (ctx, engine)
    }

    #[tokio::test]
    async fn remember_recall_forget_roundtrip() {
        let (ctx, _e) = ctx_with_engine();
        RememberTool
            .execute(&ctx, r#"{"key":"greeting","value":"hello from the rust language"}"#)
            .await
            .unwrap();
        let out = RecallTool.execute(&ctx, r#"{"query":"rust","top_k":5}"#).await.unwrap();
        assert!(out.contains("greeting"), "recall should find it: {out}");
        let f = ForgetTool.execute(&ctx, r#"{"key":"greeting"}"#).await.unwrap();
        assert!(f.contains("removed"));
    }

    #[tokio::test]
    async fn degrades_without_handle() {
        let ctx = ToolContext::new("t");
        let out = RecallTool.execute(&ctx, r#"{"query":"x"}"#).await.unwrap();
        assert!(out.contains("未启用"), "should degrade: {out}");
    }
}
```

- [ ] **Step 2: 运行测试确认通过**

Run: `cargo test -p deepseeknova-tools memory:: -- --nocapture`
Expected: PASS（2 个新测试；旧的私有 store/BM25 测试已随重写移除）。

- [ ] **Step 3: 编译整个 tools crate（确认无残留引用）**

Run: `cargo check -p deepseeknova-tools`
Expected: 通过。若报 `all_builtin_tools` 引用 `RememberTool/RecallTool/ForgetTool` 失败，说明导出名未变——它们保持同名，无需改 `lib.rs`。

- [ ] **Step 4: 提交**

```bash
git add crates/deepseeknova-tools/src/memory.rs
git commit -m "fix(tools/memory): back remember/recall/forget with persistent MemoryEngine"
```


---

## Task 6: Agent 起点召回注入 + 结束沉淀钩子

**Files:**
- Modify: `crates/deepseeknova-agent/src/agent.rs`

均为对现有文件的精确插入。`validate_replay_invariant` 仅拒绝孤儿 tool 结果与缺失 load-bearing reasoning——新增的普通 User 消息零违规。

- [ ] **Step 1: 加导入**

在 `agent.rs` 顶部 `use deepseeknova_core::{...}` 之后追加：

```rust
use deepseeknova_core::memory::skill::{TaskObservation, TaskOutcome};
```

- [ ] **Step 2: 加类型别名**

在 `pub type RepoMapProvider = ...;`（约 L74）之后追加：

```rust
/// Run-start 召回提供器：给定首条用户 prompt，返回可选的"召回上下文"块。
pub type RecallProvider = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Run-end 沉淀钩子：接收本轮组装的 TaskObservation（非阻塞捕获）。
pub type DistillHook = Arc<dyn Fn(TaskObservation) + Send + Sync>;
```

- [ ] **Step 3: 加结构体字段**

在 `Agent` 结构体 `repo_map_provider: Option<RepoMapProvider>,` 之后追加：

```rust
    recall_provider: Option<RecallProvider>,
    distill_hook: Option<DistillHook>,
```

在 `Agent::new` 的初始化里 `repo_map_provider: None,` 之后追加：

```rust
            recall_provider: None,
            distill_hook: None,
```

- [ ] **Step 4: 加 builder 方法**

在 `with_repo_map_provider` 方法之后、`register_tool` 之前追加：

```rust
    /// 附加起点召回提供器。新会话时以首条 prompt 调用，返回块作为 volatile
    /// User 消息注入（不改动被缓存的 system 前缀）。
    pub fn with_recall_provider(mut self, provider: RecallProvider) -> Self {
        self.recall_provider = Some(provider);
        self
    }

    /// 附加结束沉淀钩子。循环结束后组装 TaskObservation 并调用（非阻塞捕获）。
    pub fn with_distill_hook(mut self, hook: DistillHook) -> Self {
        self.distill_hook = Some(hook);
        self
    }
```

- [ ] **Step 5: 在 run_stream 里克隆两个字段**

在 `let repo_map_provider = self.repo_map_provider.clone();`（约 L238）之后追加：

```rust
        let recall_provider = self.recall_provider.clone();
        let distill_hook = self.distill_hook.clone();
```

- [ ] **Step 6: 插入起点召回注入 + 捕获 task_text**

定位到 `if !seeded { if let Some(ref sp) = system_prompt { ... } }` 整块结束之后、`let result = run_agent_loop(` 之前，插入：

```rust
            // Run-start 召回注入（仅新会话）：作为稳定 system 前缀之后的 volatile
            // User 消息插入 —— 保住 DeepSeek-V4 前缀缓存；无 tool_calls/tool_call_id/
            // reasoning，故通过 replay 不变量校验。
            if !seeded {
                if let Some(ref rp) = recall_provider {
                    if let Some(block) = rp(&input.prompt) {
                        if !block.is_empty() {
                            memory.add_message(Message {
                                role: Role::User,
                                content: format!("<recalled-memory>\n{block}\n</recalled-memory>"),
                                name: None,
                                tool_calls: None,
                                tool_call_id: None,
                                reasoning_content: None,
                            });
                        }
                    }
                }
            }

            // 结束沉淀需要的任务文本（input 随后被移入 run_agent_loop）。
            let task_text = input.prompt.clone();
```

- [ ] **Step 7: 插入结束沉淀钩子**

定位到写回 history 的块 `if let Some(ref hist) = history { ... }` 之后、`if let Err(e) = result {` 之前，插入：

```rust
            // Run-end 沉淀（非阻塞捕获）：取消时跳过。借用 &result，不影响后续错误日志。
            if let Some(ref hook) = distill_hook {
                if !cancel.is_cancelled() {
                    let msgs = memory.get_all();
                    let tool_calls: Vec<String> = msgs
                        .iter()
                        .filter(|m| m.role == Role::Tool)
                        .filter_map(|m| m.name.clone().or_else(|| m.tool_call_id.clone()))
                        .collect();
                    let steps_taken: Vec<String> = msgs
                        .iter()
                        .filter(|m| m.role == Role::Assistant)
                        .map(|_| "step".to_string())
                        .collect();
                    let (outcome, user_feedback) = match &result {
                        Ok(()) => (TaskOutcome::Success, None),
                        Err(e) => (TaskOutcome::Failure, Some(e.to_string())),
                    };
                    hook(TaskObservation {
                        task_description: task_text.clone(),
                        tool_calls,
                        steps_taken,
                        outcome,
                        user_feedback,
                        session_id: "agent".to_string(),
                    });
                }
            }
```

- [ ] **Step 8: 写测试**

在 `agent.rs` 的 `#[cfg(test)] mod tests` 内追加（用全路径避免 `use` 冲突；如遇重复 `use` 删去即可）：

```rust
    #[tokio::test]
    async fn recall_injects_volatile_and_keeps_system_prefix() {
        use std::sync::Mutex as StdMutex;
        struct CapturingProvider {
            seen: Arc<StdMutex<Vec<Message>>>,
        }
        #[async_trait::async_trait]
        impl deepseeknova_provider::Provider for CapturingProvider {
            async fn generate(
                &self,
                _v: deepseeknova_provider::ValidatedRequest<'_>,
            ) -> anyhow::Result<Message> {
                Ok(Message {
                    role: Role::Assistant,
                    content: "done".into(),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                })
            }
            async fn stream(
                &self,
                v: deepseeknova_provider::ValidatedRequest<'_>,
            ) -> anyhow::Result<deepseeknova_core::chunk::ChunkStream> {
                *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = v.messages.to_vec();
                let chunks: Vec<anyhow::Result<deepseeknova_core::chunk::Chunk>> = vec![
                    Ok(deepseeknova_core::chunk::Chunk::TextDelta("done".into())),
                    Ok(deepseeknova_core::chunk::Chunk::Usage(
                        deepseeknova_core::chunk::Usage::default(),
                    )),
                    Ok(deepseeknova_core::chunk::Chunk::Done),
                ];
                Ok(Box::pin(tokio_stream::iter(chunks)))
            }
        }

        let seen = Arc::new(StdMutex::new(Vec::new()));
        let provider = Arc::new(CapturingProvider { seen: seen.clone() });
        let recall: RecallProvider = Arc::new(|_q: &str| Some("REMEMBERED_FACT_XYZ".to_string()));
        let agent = Agent::new(provider, 3)
            .with_system_prompt("SYSTEM_PROMPT_BASE")
            .with_recall_provider(recall);

        let mut stream = agent
            .run_stream(RunInput {
                prompt: "hi".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        let msgs = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(msgs[0].role, Role::System);
        assert!(msgs[0].content.contains("SYSTEM_PROMPT_BASE"));
        assert!(
            !msgs[0].content.contains("REMEMBERED_FACT_XYZ"),
            "recall must NOT be in the cached system prefix"
        );
        assert!(
            msgs.iter().any(|m| m.content.contains("REMEMBERED_FACT_XYZ")),
            "recall must be injected as a volatile message"
        );
    }

    #[tokio::test]
    async fn distill_hook_fires_after_run() {
        use std::sync::Mutex as StdMutex;
        let fired = Arc::new(StdMutex::new(false));
        let f2 = fired.clone();
        let hook: DistillHook = Arc::new(move |_obs| {
            *f2.lock().unwrap_or_else(|e| e.into_inner()) = true;
        });
        let agent = Agent::new(Arc::new(MockProvider::text("ok")), 3)
            .with_system_prompt("sp")
            .with_distill_hook(hook);
        let mut stream = agent
            .run_stream(RunInput {
                prompt: "do it".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}
        assert!(*fired.lock().unwrap_or_else(|e| e.into_inner()), "distill hook should fire");
    }
```

> `MockProvider` 来自 `crate::test_utils`；若测试模块尚未引入，加 `use crate::test_utils::MockProvider;`（多数现有测试已引入）。

- [ ] **Step 9: 运行测试确认通过**

Run: `cargo test -p deepseeknova-agent recall_injects_volatile_and_keeps_system_prefix distill_hook_fires_after_run -- --nocapture`
Expected: PASS（2 个）。

- [ ] **Step 10: 提交**

```bash
git add crates/deepseeknova-agent/src/agent.rs
git commit -m "feat(agent): run-start recall injection + run-end distill hook"
```

---

## Task 7: Runtime 装配记忆引擎

**Files:**
- Modify: `crates/deepseeknova-runtime/src/lib.rs`（`build_agent` 内）

- [ ] **Step 1: 记忆工具禁用（memory 关闭时）**

在 `build_agent` 里，紧跟 graph 工具禁用块（`if !config.graph.enabled { disabled.insert("search_code"); ... }`）之后追加：

```rust
    // 记忆关闭时排除记忆工具（模型看不到其 schema），与 graph 同款处理。
    if !config.memory.enabled {
        disabled.insert("remember");
        disabled.insert("recall");
        disabled.insert("forget");
    }
```

- [ ] **Step 2: 装配引擎 + 注入 handle + recall/distill**

在 `build_agent` 的 graph 装配块（`if config.graph.enabled { ... }`）之后、`Ok(agent)` 之前插入：

```rust
    // ── 记忆引擎：持久化、注入工具句柄、装配起点召回 + 结束沉淀 ──
    if config.memory.enabled {
        let db = workspace_root.join(&config.memory.db_path);
        match deepseeknova_core::memory::engine::MemoryEngine::open(&db, config.memory.redact_secrets)
        {
            Ok(engine) => {
                let handle: deepseeknova_tools::MemoryHandle = Arc::new(engine);
                agent = agent.with_extension(handle.clone());

                // 起点召回注入（token 预算内的极简块）。
                let rp = handle.clone();
                let top_k = config.memory.recall_top_k;
                let cap_chars = config.memory.recall_inject_tokens.saturating_mul(4);
                if cap_chars > 0 {
                    let recall: deepseeknova_agent::RecallProvider = Arc::new(move |query: &str| {
                        let hits = rp.recall(query, top_k).ok()?;
                        if hits.is_empty() {
                            return None;
                        }
                        let mut block = String::from("## Recalled Context\n");
                        let mut budget = cap_chars;
                        for h in &hits {
                            let snippet: String = h.entry.content.chars().take(160).collect();
                            let line = format!("- [{}] {}\n", h.entry.id, snippet);
                            if line.len() > budget {
                                break;
                            }
                            budget -= line.len();
                            block.push_str(&line);
                        }
                        Some(block)
                    });
                    agent = agent.with_recall_provider(recall);
                }

                // 结束沉淀钩子（启发式，无 LLM）。
                let dh = handle.clone();
                let guards = deepseeknova_core::memory::engine::DistillGuards {
                    auto_learn: config.memory.auto_learn,
                    min_tool_calls: config.memory.min_tool_calls,
                    min_steps: config.memory.min_steps,
                    max_per_day: config.memory.max_distillations_per_day,
                    max_per_session: config.memory.max_distillations_per_session,
                };
                let distill: deepseeknova_agent::DistillHook = Arc::new(move |obs| {
                    if let Err(e) = dh.record_task(&obs, &guards) {
                        tracing::warn!("memory distill failed: {e}");
                    }
                });
                agent = agent.with_distill_hook(distill);
            }
            Err(e) => tracing::warn!("memory engine unavailable, tools will degrade: {e}"),
        }
    }
```

- [ ] **Step 3: 写测试（引擎装配 + 工具开关）**

在 `runtime/src/lib.rs` 的 `mod tests` 内追加：

```rust
    #[tokio::test]
    async fn build_agent_registers_memory_tools_when_enabled() {
        let mut config = Config::default();
        config.memory.enabled = true;
        config.graph.enabled = false;
        let root = std::env::temp_dir().join(format!("dnv-mem-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, root.clone(), provider, 5, None).unwrap();
        let names = agent.tool_names();
        assert!(names.iter().any(|n| n == "recall"));
        assert!(names.iter().any(|n| n == "remember"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn build_agent_skips_memory_tools_when_disabled() {
        let mut config = Config::default();
        config.memory.enabled = false;
        config.graph.enabled = false;
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None).unwrap();
        assert!(!agent.tool_names().iter().any(|n| n == "recall"));
    }
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p deepseeknova-runtime build_agent_registers_memory_tools_when_enabled build_agent_skips_memory_tools_when_disabled -- --nocapture`
Expected: PASS（2 个）。

- [ ] **Step 5: 提交**

```bash
git add crates/deepseeknova-runtime/src/lib.rs
git commit -m "feat(runtime): wire MemoryEngine (handle + recall injection + distill hook)"
```

---

## Task 8: CLI `memory` 子命令（可审查性前置）

**Files:**
- Modify: `crates/deepseeknova-cli/src/cli.rs`（加 `Memory` 变体 + `MemoryAction`）
- Modify: `crates/deepseeknova-cli/src/main.rs`（dispatch）

- [ ] **Step 1: 加子命令定义**

在 `cli.rs` 的 `enum Commands` 里，`Init,` 之前追加：

```rust
    /// 记忆库管理（查看/检索/删除/统计）。
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
    },
```

在 `cli.rs` 文件末尾追加：

```rust
#[derive(Subcommand)]
pub enum MemoryAction {
    /// 列出某类记忆（task/skill/user_profile）。
    List {
        #[arg(long, default_value = "task")]
        category: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// 按相关度检索记忆。
    Search { query: Vec<String> },
    /// 按 id/key 删除一条记忆。
    Forget { id: String },
    /// 打印统计（召回命中率、reinforce 比例）——P2 决策依据。
    Stats,
}
```

- [ ] **Step 2: 加 dispatch 分支**

在 `main.rs` 的 `match &cli.command { ... }` 里，`Some(Commands::Config) => ...` 之后追加：

```rust
        // ── Memory (审查/检索/删除/统计) ─────────────────────────────────
        Some(Commands::Memory { action }) => {
            use deepseeknova_core::memory::store::MemoryCategory;
            let db = std::env::current_dir()
                .unwrap_or_default()
                .join(&config.memory.db_path);
            let engine = deepseeknova_core::memory::engine::MemoryEngine::open(
                &db,
                config.memory.redact_secrets,
            )?;
            match action {
                cli::MemoryAction::List { category, limit } => {
                    let cat = match category.as_str() {
                        "skill" => MemoryCategory::Skill,
                        "user_profile" => MemoryCategory::UserProfile,
                        _ => MemoryCategory::Task,
                    };
                    for e in engine.list(cat)?.into_iter().take(*limit) {
                        let preview: String = e.content.chars().take(100).collect();
                        println!("[{}] ({}) {}", e.id, e.source, preview);
                    }
                }
                cli::MemoryAction::Search { query } => {
                    let q = query.join(" ");
                    for (i, r) in engine.recall(&q, 10)?.iter().enumerate() {
                        let preview: String = r.entry.content.chars().take(120).collect();
                        println!("{}. [{}] {}", i + 1, r.entry.id, preview);
                    }
                }
                cli::MemoryAction::Forget { id } => {
                    println!("{}", if engine.forget(id)? { "removed" } else { "not found" });
                }
                cli::MemoryAction::Stats => {
                    let s = engine.stats()?;
                    println!(
                        "total={} recall_hit_rate={:.2} reinforce_ratio={:.2}",
                        s.total, s.recall_hit_rate, s.reinforce_ratio
                    );
                }
            }
        }
```

- [ ] **Step 3: 编译确认**

Run: `cargo check -p deepseeknova-cli`
Expected: 通过（clap 自动生成 `deepseeknova memory <list|search|forget|stats>`）。

- [ ] **Step 4: 手动冒烟（可选）**

Run: `cargo run -p deepseeknova-cli -- memory stats`
Expected: 打印 `total=0 recall_hit_rate=0.00 reinforce_ratio=0.00`（空库）。

- [ ] **Step 5: 提交**

```bash
git add crates/deepseeknova-cli/src/cli.rs crates/deepseeknova-cli/src/main.rs
git commit -m "feat(cli): memory list/search/forget/stats for auditability"
```


---

## Task 9: 集成测试（持久化跨重启 + 并发写）

**Files:**
- Create: `crates/deepseeknova-core/tests/memory_engine.rs`

验证头号 bug 修复（重启后记忆仍在）与并发写安全（多 agent/子 agent 共享同一 handle 的真实场景）。

- [ ] **Step 1: 写集成测试文件**

Create `crates/deepseeknova-core/tests/memory_engine.rs`：

```rust
//! 集成：记忆跨重启持久 + 进程内并发写安全。

use deepseeknova_core::memory::engine::MemoryEngine;
use deepseeknova_core::memory::store::MemoryCategory;
use std::sync::Arc;

fn temp_db() -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("dnv-mem-it-{}-{}.db", std::process::id(), nanos))
}

#[test]
fn memory_persists_across_reopen() {
    let path = temp_db();
    {
        let eng = MemoryEngine::open(&path, true).unwrap();
        eng.remember("fact-1", "the build uses cargo make check", vec!["build".into()])
            .unwrap();
    } // drop → 连接关闭
    {
        let eng = MemoryEngine::open(&path, true).unwrap();
        let hits = eng.recall("cargo make check", 5).unwrap();
        assert!(
            hits.iter().any(|h| h.entry.id == "fact-1"),
            "memory must survive reopen (this is the core bug fix)"
        );
    }
    let _ = std::fs::remove_file(&path);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_writes_do_not_deadlock_or_lose() {
    let path = temp_db();
    let eng = Arc::new(MemoryEngine::open(&path, true).unwrap());
    let mut handles = Vec::new();
    for i in 0..20 {
        let e = eng.clone();
        handles.push(tokio::spawn(async move {
            e.remember(&format!("k{i}"), &format!("value number {i}"), vec![])
                .unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    let count = eng.list(MemoryCategory::Task).unwrap().len();
    assert_eq!(count, 20, "all concurrent writes must persist without loss");
    let _ = std::fs::remove_file(&path);
}
```

- [ ] **Step 2: 运行集成测试确认通过**

Run: `cargo test -p deepseeknova-core --test memory_engine -- --nocapture`
Expected: PASS（2 个）。

- [ ] **Step 3: 提交**

```bash
git add crates/deepseeknova-core/tests/memory_engine.rs
git commit -m "test(core/memory): persistence-across-restart + concurrent-write integration"
```

---

## Task 10: 全量验收

- [ ] **Step 1: 全库检查**

Run: `make check`
Expected: fmt + clippy(`-D warnings`) + test + doc 全绿（不含 desktop）。若 clippy 报 `too_many_arguments` 之类，遵循被改文件既有 `#![allow(...)]` 风格处理。

- [ ] **Step 2: 端到端冒烟（真实 provider，需 API key）**

Run: `cargo run -p deepseeknova-cli -- run "读取 README 顶部并总结三点"`（任务够格时）
随后 Run: `cargo run -p deepseeknova-cli -- memory stats`
Expected: 首个任务结束后 `total` > 0（自动捕获了任务摘要）；下一次 `run` 起点会注入 `## Recalled Context`。

- [ ] **Step 3: 最终提交（若有格式化改动）**

```bash
git add -A
git commit -m "chore(memory): P1 closed-loop learning engine complete"
```

---

## 偏差与取舍（执行前请知悉）

1. **P1 沉淀为启发式、无 LLM**：结束时确定性地捕获"任务摘要 + 失败教训"（脱敏、护栏、成本上限、去重），不做 LLM 技能合成。理由：可确定性测试、零额外 token、先用 §可观测性数据验证"简单记忆是否被复用"。LLM 合成为 P2。
2. **错误类型沿用 `anyhow`**：`store.rs`/`engine.rs` 保持与现有 memory 模块一致的 `anyhow::Result`。spec §13 提到的自定义 `MemoryError`（thiserror）延后——现有 store 全用 anyhow，P1 引入会带来跨方法签名的额外 churn，且不影响"优雅降级"行为（工具层已转友好文字、runtime 层 warn）。若需严格对齐 AGENTS.md 的 thiserror 约定，可作为独立小任务补做。
3. **去重用内容哈希 id**（`DefaultHasher`，确定性）：相同摘要 → 同 id → upsert，避免重复；语义近似去重（cosine 阈值）随 P2 向量层引入。
4. **Embedder / 向量列**：`memory_meta` 已建 `embedding/embed_dim/embed_model` 列但 P1 不写入；为 P2 预留，避免二次迁移。

## Spec 覆盖矩阵

| Spec 节 | P1 落点 | 状态 |
|---|---|---|
| §3 MemoryEngine / 工具改接 / runtime / CLI / config | Task 4/5/7/8/1 | ✓ |
| §3 Embedder trait / local / remote | — | P2 延后 |
| §4 memory_meta + WAL + 计数表 | Task 3 | ✓ |
| §4 embed_model 迁移（懒重算） | 列已预留 | P2 |
| §5 召回 IN（注入 + 按需工具 + record_recall） | Task 6/7/5/4 | ✓ |
| §6 沉淀 OUT（护栏 + 成本上限 + redaction + 去重） | Task 4/6/7 | ✓（启发式） |
| §6 LLM 合成技能/画像 | — | P2 延后 |
| §7 语义层 | — | P2 延后 |
| §8 lifecycle 持久化 | Task 3/4 | ✓ |
| §8 分层归纳 consolidation | — | P3 延后 |
| §9 CLI 审查入口 | Task 8 | ✓ |
| §10 可观测性 + P2 门槛数据 | Task 3/4/8 | ✓ |
| §11 token 护栏 | Task 4/5/6/7 | ✓ |
| §12 `[memory]` 配置 | Task 1 | ✓ |
| §13 优雅降级 | Task 5/7 | ✓（anyhow，见偏差 2） |
| §14 测试（含并发/持久/注入前缀稳定） | Task 3/4/5/6/9 | ✓ |

## 自审确认（writing-plans 自检）

- **Spec 覆盖**：每条 P1 需求均有对应 Task；P2/P3 项已在矩阵显式标注延后。
- **占位符扫描**：无 TODO/TBD；每个改动步骤含完整代码与确切命令、预期输出。
- **类型一致性**：`MemoryEngine`/`DistillGuards`/`MemoryStats`/`MemoryHandle`/`RecallProvider`/`DistillHook` 及 store 新方法在定义处与调用处（tools/runtime/cli/tests）签名逐一对齐。

---

## 执行方式

**Plan complete and saved to `docs/superpowers/plans/2026-07-28-closed-loop-memory-engine-p1.md`. 两种执行方式：**

**1. 子代理驱动（推荐）** —— 每个 Task 派新子代理实现，Task 间由我审查，迭代快、隔离好。

**2. 内联执行** —— 在本会话按 executing-plans 分批执行，带检查点复核。

**选哪种？**
