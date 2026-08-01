# 多模型指针与成本分账实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 按角色（main/task/compact/quick）路由模型，并按 模型×角色 分账 token 与美元成本估算。

**Architecture:** config 层新增 `[model_pointers]` 与模型单价字段（加载期校验）；provider 层新增 `cost.rs`（ModelRole/CostLedger/MeteredProvider 计量装饰器）与 `router.rs`（ModelRouter，惰性构建 + (provider,模型,effort) 缓存键）；agent 层给 SubAgentRunner 增加 compact 专用 provider；runtime 委派引擎接 task provider；CLI 扩展 `/model` 并新增 `/cost`。计量通过 MeteredProvider 装饰器在流中拦截 `Chunk::Usage` 自动完成，不改 `Provider` trait 与 `Chunk` 协议。

**Tech Stack:** Rust（thiserror/anyhow、tokio、serde/toml）、现有 workspace crate：deepseeknova-config / provider / agent / runtime / cli。

**Spec:** `docs/superpowers/specs/2026-07-29-model-pointers-cost-ledger-design.md`

**验证约定：** 每个任务内先写失败测试再实现（TDD）；每任务一次提交；全部完成后 `make check` 全量回归（本变更跨 crate，强制）。

---

### Task 1: config — 模型指针 + 单价字段 + 加载期校验

**Files:**
- Modify: `crates/deepseeknova-config/src/lib.rs`
- Test: `crates/deepseeknova-config/tests/integration.rs`

- [ ] **Step 1: 写失败测试**

在 `crates/deepseeknova-config/tests/integration.rs` 末尾追加：

```rust
// ---------------------------------------------------------------------------
// Model pointers & pricing (multi-model orchestration)
// ---------------------------------------------------------------------------

fn pointer_config_toml() -> &'static str {
    r#"
        [[providers]]
        name = "deepseek"
        kind = "openai"

        [[models]]
        name = "big"
        provider = "deepseek"
        input_price_per_mtok = 0.28
        output_price_per_mtok = 0.42

        [[models]]
        name = "small"
        provider = "deepseek"

        [model_pointers]
        main = "big"
        task = "small"
    "#
}

#[test]
fn model_pointers_parse_and_validate() {
    let cfg: deepseeknova_config::Config = toml::from_str(pointer_config_toml()).unwrap();
    assert_eq!(cfg.model_pointers.main.as_deref(), Some("big"));
    assert_eq!(cfg.model_pointers.task.as_deref(), Some("small"));
    assert_eq!(cfg.model_pointers.compact, None);
    assert_eq!(cfg.model_pointers.quick, None);
    let big = cfg.find_model("big").unwrap();
    assert_eq!(big.input_price_per_mtok, Some(0.28));
    assert_eq!(big.output_price_per_mtok, Some(0.42));
    assert_eq!(big.cache_hit_price_per_mtok, None);
    cfg.validate().unwrap();
}

#[test]
fn dangling_pointer_rejected() {
    let mut cfg: deepseeknova_config::Config = toml::from_str(pointer_config_toml()).unwrap();
    cfg.model_pointers.quick = Some("no-such-model".to_string());
    let err = cfg.validate().unwrap_err().to_string();
    assert!(err.contains("quick"), "error should name the pointer: {err}");
    assert!(err.contains("no-such-model"), "error should name the model: {err}");
    assert!(err.contains("big"), "error should list candidates: {err}");
}

#[test]
fn negative_price_rejected() {
    let mut cfg: deepseeknova_config::Config = toml::from_str(pointer_config_toml()).unwrap();
    cfg.models[0].input_price_per_mtok = Some(-1.0);
    assert!(cfg.validate().is_err());
}

#[test]
fn model_pointers_merge_project_over_user() {
    let mut user: deepseeknova_config::Config = toml::from_str(pointer_config_toml()).unwrap();
    let mut project = deepseeknova_config::Config::default();
    project.model_pointers.main = Some("small".to_string());
    user.merge(project);
    // 项目层覆盖 main；未设置的 task 保留用户层的值
    assert_eq!(user.model_pointers.main.as_deref(), Some("small"));
    assert_eq!(user.model_pointers.task.as_deref(), Some("small"));
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p deepseeknova-config --test integration model_pointer`
Expected: 编译失败 —— `model_pointers` 字段与 `validate` 方法不存在。

- [ ] **Step 3: 实现**

`crates/deepseeknova-config/src/lib.rs` 四处修改：

(a) `Config` 结构体（`telemetry` 字段之后、结构体闭合前）追加字段：

```rust
    /// Role-based model pointers (main/task/compact/quick).
    #[serde(default)]
    pub model_pointers: ModelPointersConfig,
```

(b) `ModelConfig` 结构体（`planner_only` 字段之后）追加字段：

```rust
    /// Input (prompt) price in USD per 1M tokens. Unset = cost not estimated.
    #[serde(default)]
    pub input_price_per_mtok: Option<f64>,

    /// Output (completion, incl. reasoning) price in USD per 1M tokens.
    #[serde(default)]
    pub output_price_per_mtok: Option<f64>,

    /// Prompt-cache-hit price in USD per 1M tokens. Unset = falls back to
    /// `input_price_per_mtok` when estimating.
    #[serde(default)]
    pub cache_hit_price_per_mtok: Option<f64>,
```

(c) `ModelConfig` 定义之后新增段落：

```rust
// ---------------------------------------------------------------------------
// Model pointers — role-based model routing (Kode-style main/task/compact/quick)
// ---------------------------------------------------------------------------

/// Role-based model pointers. Each role optionally names an entry in
/// `[[models]]`. Unset roles fall back to `main`; an unset `main` falls back
/// to the legacy default-provider resolution, so zero-config behaviour is
/// unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelPointersConfig {
    /// Primary conversation model.
    #[serde(default)]
    pub main: Option<String>,
    /// Sub-agent / delegation model.
    #[serde(default)]
    pub task: Option<String>,
    /// History-compaction (summarize) model.
    #[serde(default)]
    pub compact: Option<String>,
    /// Fast utility model (titles, classification).
    #[serde(default)]
    pub quick: Option<String>,
}

impl ModelPointersConfig {
    fn merge(&mut self, other: ModelPointersConfig) {
        if other.main.is_some() {
            self.main = other.main;
        }
        if other.task.is_some() {
            self.task = other.task;
        }
        if other.compact.is_some() {
            self.compact = other.compact;
        }
        if other.quick.is_some() {
            self.quick = other.quick;
        }
    }

    /// Iterate (role-name, pointer) pairs for validation and routing.
    pub fn entries(&self) -> [(&'static str, &Option<String>); 4] {
        [
            ("main", &self.main),
            ("task", &self.task),
            ("compact", &self.compact),
            ("quick", &self.quick),
        ]
    }
}
```

(d) `impl Config`（`resolve_provider_for_model` 之后）追加校验方法；并在 `merge` 中 `self.telemetry.merge(other.telemetry);` 之后追加 `self.model_pointers.merge(other.model_pointers);`；在 `load()` 的 `Ok(config)` 之前追加 `config.validate()?;`：

```rust
    /// Validate cross-references: model pointers must name a defined model,
    /// and prices must be non-negative. Called by [`Config::load`]; callers
    /// constructing configs programmatically may call it directly.
    pub fn validate(&self) -> anyhow::Result<()> {
        let names: Vec<&str> = self.models.iter().map(|m| m.name.as_str()).collect();
        for (role, ptr) in self.model_pointers.entries() {
            if let Some(model) = ptr {
                if !names.contains(&model.as_str()) {
                    anyhow::bail!(
                        "model_pointers.{role} points to unknown model '{model}' \
                         (known models: {})",
                        names.join(", ")
                    );
                }
            }
        }
        for m in &self.models {
            for (field, price) in [
                ("input_price_per_mtok", m.input_price_per_mtok),
                ("output_price_per_mtok", m.output_price_per_mtok),
                ("cache_hit_price_per_mtok", m.cache_hit_price_per_mtok),
            ] {
                if let Some(p) = price {
                    if p < 0.0 {
                        anyhow::bail!("models.{}.{field} must be >= 0, got {p}", m.name);
                    }
                }
            }
        }
        Ok(())
    }
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p deepseeknova-config`
Expected: 全部 PASS（含既有测试，确认 `Default` 派生未破坏反序列化）。

- [ ] **Step 5: Commit**

```bash
git add crates/deepseeknova-config
git commit -m "feat(config): model_pointers 段与模型单价字段（加载期校验）"
```

---

### Task 2: provider factory — 按模型名 override 构建

**Files:**
- Modify: `crates/deepseeknova-provider/src/lib.rs`（`pub mod factory` 内）

- [ ] **Step 1: 写失败测试**

在 `crates/deepseeknova-provider/src/lib.rs` 文件末尾的测试模块（若无则新建 `#[cfg(test)] mod factory_tests`）追加：

```rust
#[cfg(test)]
mod factory_tests {
    use deepseeknova_config::ProviderConfig;

    fn provider_cfg() -> ProviderConfig {
        toml::from_str(
            r#"
            name = "deepseek"
            kind = "openai"
            api_key_env = "DPNOVA_TEST_KEY"
        "#,
        )
        .unwrap()
    }

    #[test]
    fn create_provider_with_model_overrides_model() {
        std::env::set_var("DPNOVA_TEST_KEY", "test");
        let cfg = provider_cfg();
        // 构建成功即可 —— 模型名注入路径由 router 缓存键测试进一步覆盖
        let p = crate::factory::create_provider_with_model(&cfg, "my-model", None);
        assert!(p.is_ok(), "{:?}", p.err());
    }
}
```

若 provider 的 `Cargo.toml` `[dev-dependencies]` 无 `toml`，追加 `toml = { workspace = true }`（workspace 根已有该依赖；若根 `[workspace.dependencies]` 无 toml 则写 `toml = "0.8"` 与 config crate 版本一致）。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p deepseeknova-provider create_provider_with_model`
Expected: 编译失败 —— `create_provider_with_model` 不存在。

- [ ] **Step 3: 实现**

`pub mod factory` 内、`create_provider_for_task` 之后追加：

```rust
    /// Create a Provider for a specific model name, overriding the provider
    /// config's default `model`. Used by the ModelRouter so one provider
    /// entry can serve multiple named models.
    pub fn create_provider_with_model(
        cfg: &ProviderConfig,
        model_name: &str,
        task_classification: Option<ReasoningEffort>,
    ) -> anyhow::Result<Box<dyn Provider>> {
        let mut cfg = cfg.clone();
        cfg.model = Some(model_name.to_string());
        create_provider_for_task(&cfg, task_classification)
    }
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p deepseeknova-provider create_provider_with_model`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/deepseeknova-provider Cargo.lock
git commit -m "feat(provider): factory 支持按模型名 override 构建"
```

---

### Task 3: provider cost.rs — ModelRole / CostLedger / MeteredProvider

**Files:**
- Create: `crates/deepseeknova-provider/src/cost.rs`
- Modify: `crates/deepseeknova-provider/src/lib.rs`（加 `pub mod cost;`）

- [ ] **Step 1: 创建模块骨架 + 失败测试**

在 lib.rs 模块声明区（`pub mod anthropic;` 之后）加 `pub mod cost;`。创建 `cost.rs`，先只写测试（实现留空则编译失败，直接进 Step 2 的 RED 状态）。测试写在文件底部：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_core::chunk::Usage;

    fn usage(prompt: u32, completion: u32, cache_hit: u32) -> Usage {
        Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
            cache_hit_tokens: cache_hit,
            cache_miss_tokens: prompt.saturating_sub(cache_hit),
            reasoning_tokens: 0,
        }
    }

    #[test]
    fn ledger_aggregates_per_model_and_role() {
        let ledger = CostLedger::new();
        ledger.record(ModelRole::Main, "big", &usage(100, 50, 0));
        ledger.record(ModelRole::Main, "big", &usage(100, 50, 0));
        ledger.record(ModelRole::Task, "small", &usage(10, 5, 0));
        let report = ledger.report(&PriceTable::new());
        assert_eq!(report.rows.len(), 2);
        let main_row = report
            .rows
            .iter()
            .find(|r| r.role == ModelRole::Main)
            .unwrap();
        assert_eq!(main_row.model, "big");
        assert_eq!(main_row.bucket.prompt_tokens, 200);
        assert_eq!(main_row.bucket.completion_tokens, 100);
        assert_eq!(main_row.bucket.metered_calls, 2);
        // 无单价 → 无美元估算，但不报错
        assert!(main_row.cost_usd.is_none());
        assert!(report.total_usd.is_none());
    }

    #[test]
    fn cost_estimate_with_prices_and_cache_fallback() {
        let ledger = CostLedger::new();
        // 1M prompt（其中 0.5M cache hit）、1M completion
        ledger.record(ModelRole::Main, "big", &usage(1_000_000, 1_000_000, 500_000));
        let mut prices = PriceTable::new();
        prices.insert(
            "big".to_string(),
            ModelPrices {
                input_per_mtok: Some(2.0),
                output_per_mtok: Some(8.0),
                cache_hit_per_mtok: Some(0.2),
            },
        );
        let report = ledger.report(&prices);
        // 0.5M*2.0 + 0.5M*0.2 + 1M*8.0 = 1.0 + 0.1 + 8.0
        let cost = report.rows[0].cost_usd.unwrap();
        assert!((cost - 9.1).abs() < 1e-9, "got {cost}");
        assert!((report.total_usd.unwrap() - 9.1).abs() < 1e-9);

        // cache 单价缺失 → cache hit 按 input 单价计：1M*2.0 + 1M*8.0 = 10.0
        prices.get_mut("big").unwrap().cache_hit_per_mtok = None;
        let cost = ledger.report(&prices).rows[0].cost_usd.unwrap();
        assert!((cost - 10.0).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn partial_prices_disable_dollar_estimate() {
        let ledger = CostLedger::new();
        ledger.record(ModelRole::Quick, "small", &usage(10, 5, 0));
        let mut prices = PriceTable::new();
        prices.insert(
            "small".to_string(),
            ModelPrices {
                input_per_mtok: Some(1.0),
                output_per_mtok: None, // 缺 output 单价
                cache_hit_per_mtok: None,
            },
        );
        let report = ledger.report(&prices);
        assert!(report.rows[0].cost_usd.is_none());
    }

    #[test]
    fn unmetered_calls_are_counted() {
        let ledger = CostLedger::new();
        ledger.record_unmetered(ModelRole::Compact, "big");
        let report = ledger.report(&PriceTable::new());
        assert_eq!(report.unmetered_calls, 1);
        assert_eq!(report.rows[0].bucket.unmetered_calls, 1);
        assert_eq!(report.rows[0].bucket.metered_calls, 0);
    }

    #[tokio::test]
    async fn metered_provider_records_stream_usage() {
        use deepseeknova_core::chunk::Chunk;
        use deepseeknova_core::{Message, Role};
        use std::sync::Arc;

        struct FakeProvider {
            with_usage: bool,
        }
        #[async_trait::async_trait]
        impl crate::Provider for FakeProvider {
            async fn generate(
                &self,
                _v: crate::ValidatedRequest<'_>,
            ) -> anyhow::Result<Message> {
                Ok(Message {
                    role: Role::Assistant,
                    content: "ok".into(),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                })
            }
            async fn stream(
                &self,
                _v: crate::ValidatedRequest<'_>,
            ) -> anyhow::Result<deepseeknova_core::chunk::ChunkStream> {
                let mut chunks = vec![Ok(Chunk::TextDelta("hi".into()))];
                if self.with_usage {
                    chunks.push(Ok(Chunk::Usage(usage(7, 3, 0))));
                }
                chunks.push(Ok(Chunk::Done));
                Ok(Box::pin(tokio_stream::iter(chunks)))
            }
        }

        let msgs = vec![Message {
            role: Role::User,
            content: "hi".into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }];

        // 带 Usage 的流 → 记账
        let ledger = Arc::new(CostLedger::new());
        let p = MeteredProvider::new(
            Arc::new(FakeProvider { with_usage: true }),
            ModelRole::Task,
            "small",
            Arc::clone(&ledger),
        );
        let v = crate::ValidatedRequest::new(&msgs, &[]).unwrap();
        let mut s = p.stream(v).await.unwrap();
        use tokio_stream::StreamExt;
        while s.next().await.is_some() {}
        drop(s);
        let report = ledger.report(&PriceTable::new());
        assert_eq!(report.rows[0].bucket.prompt_tokens, 7);
        assert_eq!(report.rows[0].bucket.metered_calls, 1);
        assert_eq!(report.unmetered_calls, 0);

        // 无 Usage 的流 → 未计量
        let ledger = Arc::new(CostLedger::new());
        let p = MeteredProvider::new(
            Arc::new(FakeProvider { with_usage: false }),
            ModelRole::Task,
            "small",
            Arc::clone(&ledger),
        );
        let v = crate::ValidatedRequest::new(&msgs, &[]).unwrap();
        let mut s = p.stream(v).await.unwrap();
        while s.next().await.is_some() {}
        drop(s);
        assert_eq!(ledger.report(&PriceTable::new()).unmetered_calls, 1);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p deepseeknova-provider cost`
Expected: 编译失败 —— 类型未定义。

- [ ] **Step 3: 实现（cost.rs 顶部，测试模块之前）**

```rust
//! Per-model × per-role token accounting with optional USD estimation.
//!
//! [`MeteredProvider`] is a transparent [`Provider`] decorator: it intercepts
//! `Chunk::Usage` on the stream and records it into a shared [`CostLedger`].
//! Non-streaming `generate` calls carry no usage (Message has no usage field)
//! and are counted as *unmetered*, as are streams that end or drop without a
//! usage chunk.

use crate::{Provider, ValidatedRequest};
use async_trait::async_trait;
use deepseeknova_core::chunk::{Chunk, ChunkStream, Usage};
use deepseeknova_core::Message;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

/// The role a model plays in the agent pipeline (Kode-style pointers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelRole {
    /// Primary conversation.
    Main,
    /// Sub-agent / delegation.
    Task,
    /// History compaction (summarize).
    Compact,
    /// Fast utility operations.
    Quick,
}

impl ModelRole {
    /// Stable lowercase label, matching `[model_pointers]` keys.
    pub fn label(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Task => "task",
            Self::Compact => "compact",
            Self::Quick => "quick",
        }
    }

    /// Parse a `[model_pointers]` key / CLI role argument.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "main" => Some(Self::Main),
            "task" => Some(Self::Task),
            "compact" => Some(Self::Compact),
            "quick" => Some(Self::Quick),
            _ => None,
        }
    }
}

/// Accumulated token counts for one (role, model) pair.
#[derive(Debug, Clone, Default)]
pub struct UsageBucket {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub reasoning_tokens: u64,
    pub cache_hit_tokens: u64,
    pub cache_miss_tokens: u64,
    /// Calls that reported a usage chunk.
    pub metered_calls: u64,
    /// Calls with no usage information (non-streaming generate, interrupted
    /// or usage-less streams). No dollar estimate is attempted for these.
    pub unmetered_calls: u64,
}

/// Optional USD prices for one model, in $/1M tokens.
#[derive(Debug, Clone, Copy, Default)]
pub struct ModelPrices {
    pub input_per_mtok: Option<f64>,
    pub output_per_mtok: Option<f64>,
    /// Falls back to `input_per_mtok` when unset.
    pub cache_hit_per_mtok: Option<f64>,
}

/// Model-name → prices lookup, built from config by the router.
pub type PriceTable = HashMap<String, ModelPrices>;

/// One line of a [`CostReport`].
#[derive(Debug, Clone)]
pub struct CostRow {
    pub model: String,
    pub role: ModelRole,
    pub bucket: UsageBucket,
    /// USD estimate; `None` when input or output price is missing.
    pub cost_usd: Option<f64>,
}

/// Aggregated accounting snapshot.
#[derive(Debug, Clone, Default)]
pub struct CostReport {
    pub rows: Vec<CostRow>,
    /// Sum over rows that have an estimate; `None` when no row has one.
    pub total_usd: Option<f64>,
    /// Total unmetered calls across all rows.
    pub unmetered_calls: u64,
}

/// Thread-safe per-(role, model) token ledger shared across the run.
#[derive(Debug, Default)]
pub struct CostLedger {
    buckets: Mutex<HashMap<(ModelRole, String), UsageBucket>>,
}

impl CostLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a metered usage report for one call.
    pub fn record(&self, role: ModelRole, model: &str, usage: &Usage) {
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let b = buckets.entry((role, model.to_string())).or_default();
        b.prompt_tokens += u64::from(usage.prompt_tokens);
        b.completion_tokens += u64::from(usage.completion_tokens);
        b.reasoning_tokens += u64::from(usage.reasoning_tokens);
        b.cache_hit_tokens += u64::from(usage.cache_hit_tokens);
        b.cache_miss_tokens += u64::from(usage.cache_miss_tokens);
        b.metered_calls += 1;
    }

    /// Record a call for which no usage information was received.
    pub fn record_unmetered(&self, role: ModelRole, model: &str) {
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        buckets
            .entry((role, model.to_string()))
            .or_default()
            .unmetered_calls += 1;
    }

    /// Snapshot the ledger into a report, estimating USD where prices exist.
    pub fn report(&self, prices: &PriceTable) -> CostReport {
        let buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows: Vec<CostRow> = buckets
            .iter()
            .map(|((role, model), bucket)| CostRow {
                model: model.clone(),
                role: *role,
                bucket: bucket.clone(),
                cost_usd: prices.get(model).and_then(|p| row_cost(bucket, p)),
            })
            .collect();
        rows.sort_by(|a, b| (&a.model, a.role.label()).cmp(&(&b.model, b.role.label())));
        let estimates: Vec<f64> = rows.iter().filter_map(|r| r.cost_usd).collect();
        let total_usd = if estimates.is_empty() {
            None
        } else {
            Some(estimates.iter().sum())
        };
        let unmetered_calls = rows.iter().map(|r| r.bucket.unmetered_calls).sum();
        CostReport {
            rows,
            total_usd,
            unmetered_calls,
        }
    }
}

/// USD estimate for one bucket. Requires input + output prices; cache-hit
/// tokens use the cache price when set, else the input price. Reasoning
/// tokens are already included in `completion_tokens` by providers.
fn row_cost(b: &UsageBucket, p: &ModelPrices) -> Option<f64> {
    let input = p.input_per_mtok?;
    let output = p.output_per_mtok?;
    let cache = p.cache_hit_per_mtok.unwrap_or(input);
    let hit = b.cache_hit_tokens as f64;
    let uncached_prompt = (b.prompt_tokens as f64 - hit).max(0.0);
    Some(
        (uncached_prompt * input + hit * cache + b.completion_tokens as f64 * output)
            / 1_000_000.0,
    )
}

// ---------------------------------------------------------------------------
// MeteredProvider — transparent accounting decorator
// ---------------------------------------------------------------------------

/// Wraps a [`Provider`] and records usage under a fixed (role, model) into a
/// shared [`CostLedger`]. Streaming calls are metered from `Chunk::Usage`;
/// `generate` calls are counted as unmetered (no usage on `Message`).
pub struct MeteredProvider {
    inner: Arc<dyn Provider>,
    role: ModelRole,
    model: String,
    ledger: Arc<CostLedger>,
}

impl MeteredProvider {
    pub fn new(
        inner: Arc<dyn Provider>,
        role: ModelRole,
        model: impl Into<String>,
        ledger: Arc<CostLedger>,
    ) -> Self {
        Self {
            inner,
            role,
            model: model.into(),
            ledger,
        }
    }
}

#[async_trait]
impl Provider for MeteredProvider {
    async fn generate(&self, validated: ValidatedRequest<'_>) -> anyhow::Result<Message> {
        let out = self.inner.generate(validated).await?;
        self.ledger.record_unmetered(self.role, &self.model);
        Ok(out)
    }

    async fn stream(&self, validated: ValidatedRequest<'_>) -> anyhow::Result<ChunkStream> {
        let inner = self.inner.stream(validated).await?;
        Ok(Box::pin(MeteredStream {
            inner,
            role: self.role,
            model: self.model.clone(),
            ledger: Arc::clone(&self.ledger),
            saw_usage: false,
            closed: false,
        }))
    }
}

/// Stream adapter: forwards chunks, records `Chunk::Usage`, and counts the
/// call as unmetered if the stream ends or is dropped without one.
struct MeteredStream {
    inner: ChunkStream,
    role: ModelRole,
    model: String,
    ledger: Arc<CostLedger>,
    saw_usage: bool,
    closed: bool,
}

impl tokio_stream::Stream for MeteredStream {
    type Item = anyhow::Result<Chunk>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(item)) => {
                if let Ok(Chunk::Usage(u)) = &item {
                    this.saw_usage = true;
                    this.ledger.record(this.role, &this.model, u);
                }
                Poll::Ready(Some(item))
            }
            Poll::Ready(None) => {
                if !this.closed {
                    this.closed = true;
                    if !this.saw_usage {
                        this.ledger.record_unmetered(this.role, &this.model);
                    }
                }
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for MeteredStream {
    fn drop(&mut self) {
        // Interrupted mid-stream (e.g. cancellation) without usage → unmetered.
        if !self.closed && !self.saw_usage {
            self.ledger.record_unmetered(self.role, &self.model);
        }
    }
}
```

注意：若 `#![cfg_attr(not(test), deny(clippy::unwrap_used, ...))]` 报警 `buckets.lock().unwrap...`，已用 `unwrap_or_else(|e| e.into_inner())` 规避毒锁 panic，无需 `unwrap`。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p deepseeknova-provider cost`
Expected: 6 个测试全部 PASS。

- [ ] **Step 5: Commit**

```bash
git add crates/deepseeknova-provider
git commit -m "feat(provider): CostLedger 按模型×角色分账与 MeteredProvider 计量装饰器"
```

---

### Task 4: provider router.rs — ModelRouter

**Files:**
- Create: `crates/deepseeknova-provider/src/router.rs`
- Modify: `crates/deepseeknova-provider/src/lib.rs`（加 `pub mod router;`）

- [ ] **Step 1: 写失败测试**（router.rs 底部）

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cost::{CostLedger, ModelRole};
    use std::sync::Arc;

    fn router() -> ModelRouter {
        std::env::set_var("DPNOVA_ROUTER_TEST_KEY", "test");
        let cfg: deepseeknova_config::Config = toml::from_str(
            r#"
            [[providers]]
            name = "deepseek"
            kind = "openai"
            api_key_env = "DPNOVA_ROUTER_TEST_KEY"

            [[models]]
            name = "big"
            provider = "deepseek"
            input_price_per_mtok = 2.0
            output_price_per_mtok = 8.0

            [[models]]
            name = "small"
            provider = "deepseek"

            [model_pointers]
            main = "big"
            task = "small"
        "#,
        )
        .unwrap();
        ModelRouter::from_config(&cfg, Arc::new(CostLedger::new())).unwrap()
    }

    #[test]
    fn pointer_resolution_and_fallback() {
        let r = router();
        assert_eq!(r.pointer(ModelRole::Main).as_deref(), Some("big"));
        assert_eq!(r.pointer(ModelRole::Task).as_deref(), Some("small"));
        // 未配置的角色回落 main
        assert_eq!(r.pointer(ModelRole::Compact).as_deref(), Some("big"));
        assert_eq!(r.pointer(ModelRole::Quick).as_deref(), Some("big"));
    }

    #[test]
    fn cache_isolates_models_and_shares_same_model() {
        let r = router();
        r.provider_for(ModelRole::Main, None).unwrap();
        r.provider_for(ModelRole::Task, None).unwrap();
        assert_eq!(r.cached_instances(), 2, "big 与 small 各一实例");
        // Compact 回落 main → 命中 big 缓存，不新增
        r.provider_for(ModelRole::Compact, None).unwrap();
        assert_eq!(r.cached_instances(), 2);
    }

    #[test]
    fn hot_switch_validates_and_takes_effect() {
        let r = router();
        assert!(r.set_pointer(ModelRole::Quick, "no-such").is_err());
        r.set_pointer(ModelRole::Quick, "small").unwrap();
        assert_eq!(r.pointer(ModelRole::Quick).as_deref(), Some("small"));
    }

    #[test]
    fn price_table_from_config() {
        let r = router();
        let t = r.price_table();
        assert_eq!(t.get("big").unwrap().input_per_mtok, Some(2.0));
        assert!(t.get("small").unwrap().input_per_mtok.is_none());
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p deepseeknova-provider router`
Expected: 编译失败。

- [ ] **Step 3: 实现（router.rs 顶部）**

```rust
//! Role → model → provider routing (Kode-style model pointers).
//!
//! [`ModelRouter`] resolves the `[model_pointers]` config section into
//! lazily-built, cached provider instances. Every returned provider is
//! wrapped in a [`MeteredProvider`](crate::cost::MeteredProvider) so token
//! accounting happens transparently.

use crate::cost::{CostLedger, MeteredProvider, ModelPrices, ModelRole, PriceTable};
use crate::factory::{self, ReasoningEffort};
use crate::Provider;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

/// Cache key: (provider name, model name, effort label). Effort participates
/// because it changes provider construction (thinking mode / effort string).
type CacheKey = (String, String, &'static str);

fn effort_key(effort: Option<ReasoningEffort>) -> &'static str {
    match effort {
        None => "config",
        Some(ReasoningEffort::Disabled) => "disabled",
        Some(ReasoningEffort::High) => "high",
        Some(ReasoningEffort::Max) => "max",
    }
}

/// Resolves model pointers to cached provider instances.
pub struct ModelRouter {
    config: deepseeknova_config::Config,
    pointers: RwLock<HashMap<ModelRole, String>>,
    cache: Mutex<HashMap<CacheKey, Arc<dyn Provider>>>,
    ledger: Arc<CostLedger>,
}

impl ModelRouter {
    /// Build from config. Re-runs [`Config::validate`] so programmatically
    /// constructed configs get the same dangling-pointer guarantees as
    /// `Config::load`.
    pub fn from_config(
        config: &deepseeknova_config::Config,
        ledger: Arc<CostLedger>,
    ) -> anyhow::Result<Self> {
        config.validate()?;
        let mut pointers = HashMap::new();
        for (role_name, ptr) in config.model_pointers.entries() {
            if let (Some(role), Some(model)) = (ModelRole::parse(role_name), ptr.as_ref()) {
                pointers.insert(role, model.clone());
            }
        }
        Ok(Self {
            config: config.clone(),
            pointers: RwLock::new(pointers),
            cache: Mutex::new(HashMap::new()),
            ledger,
        })
    }

    /// The shared cost ledger fed by all providers this router hands out.
    pub fn ledger(&self) -> Arc<CostLedger> {
        Arc::clone(&self.ledger)
    }

    /// Current model for a role: role pointer → `main` pointer → `None`
    /// (caller falls back to legacy default-provider resolution).
    pub fn pointer(&self, role: ModelRole) -> Option<String> {
        let ptrs = self.pointers.read().unwrap_or_else(|e| e.into_inner());
        ptrs.get(&role)
            .or_else(|| {
                if role != ModelRole::Main {
                    ptrs.get(&ModelRole::Main)
                } else {
                    None
                }
            })
            .cloned()
    }

    /// In-session hot switch (memory only, never persisted). Fails when the
    /// model is not defined in `[[models]]`.
    pub fn set_pointer(&self, role: ModelRole, model: &str) -> anyhow::Result<()> {
        if self.config.find_model(model).is_none() {
            let known: Vec<&str> = self.config.models.iter().map(|m| m.name.as_str()).collect();
            anyhow::bail!(
                "unknown model '{model}' for pointer '{}' (known models: {})",
                role.label(),
                known.join(", ")
            );
        }
        self.pointers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(role, model.to_string());
        Ok(())
    }

    /// Metered provider for a role. Pointer-less roles use the legacy
    /// default (first provider entry, its own default model).
    pub fn provider_for(
        &self,
        role: ModelRole,
        effort: Option<ReasoningEffort>,
    ) -> anyhow::Result<Arc<dyn Provider>> {
        match self.pointer(role) {
            Some(model) => self.provider_for_model(&model, role, effort),
            None => self.default_provider(role, effort),
        }
    }

    /// Metered provider for an explicit model name (e.g. `/model switch`),
    /// still accounted under `role`.
    pub fn provider_for_model(
        &self,
        model_name: &str,
        role: ModelRole,
        effort: Option<ReasoningEffort>,
    ) -> anyhow::Result<Arc<dyn Provider>> {
        let pcfg = self
            .config
            .resolve_provider_for_model(model_name)
            .ok_or_else(|| anyhow::anyhow!("no provider found for model '{model_name}'"))?;
        let key: CacheKey = (pcfg.name.clone(), model_name.to_string(), effort_key(effort));
        let raw = self.cached_or_build(key, || {
            Ok(factory::create_provider_with_model(pcfg, model_name, effort)?.into())
        })?;
        Ok(Arc::new(MeteredProvider::new(
            raw,
            role,
            model_name,
            Arc::clone(&self.ledger),
        )))
    }

    fn default_provider(
        &self,
        role: ModelRole,
        effort: Option<ReasoningEffort>,
    ) -> anyhow::Result<Arc<dyn Provider>> {
        let pcfg = self
            .config
            .providers
            .first()
            .ok_or_else(|| anyhow::anyhow!("no providers configured"))?;
        let model_label = pcfg.model.clone().unwrap_or_else(|| "(default)".to_string());
        let key: CacheKey = (pcfg.name.clone(), model_label.clone(), effort_key(effort));
        let raw = self.cached_or_build(key, || {
            Ok(factory::create_provider_for_task(pcfg, effort)?.into())
        })?;
        Ok(Arc::new(MeteredProvider::new(
            raw,
            role,
            model_label,
            Arc::clone(&self.ledger),
        )))
    }

    fn cached_or_build(
        &self,
        key: CacheKey,
        build: impl FnOnce() -> anyhow::Result<Arc<dyn Provider>>,
    ) -> anyhow::Result<Arc<dyn Provider>> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = cache.get(&key) {
            return Ok(Arc::clone(p));
        }
        let built = build()?;
        cache.insert(key, Arc::clone(&built));
        Ok(built)
    }

    /// Number of distinct cached raw provider instances (for tests/insight).
    pub fn cached_instances(&self) -> usize {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Price table for [`CostLedger::report`], built from `[[models]]`.
    pub fn price_table(&self) -> PriceTable {
        self.config
            .models
            .iter()
            .map(|m| {
                (
                    m.name.clone(),
                    ModelPrices {
                        input_per_mtok: m.input_price_per_mtok,
                        output_per_mtok: m.output_price_per_mtok,
                        cache_hit_per_mtok: m.cache_hit_price_per_mtok,
                    },
                )
            })
            .collect()
    }
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p deepseeknova-provider router`
Expected: 4 个测试 PASS。再跑 `cargo test -p deepseeknova-provider` 全量确认无回归。

- [ ] **Step 5: Commit**

```bash
git add crates/deepseeknova-provider
git commit -m "feat(provider): ModelRouter 角色指针路由与实例缓存"
```


---

### Task 5: agent — SubAgentRunner 压缩走 Compact provider

**Files:**
- Modify: `crates/deepseeknova-agent/src/sub_agent.rs`

- [ ] **Step 1: 写失败测试**

在 `sub_agent.rs` 底部现有测试模块内追加（沿用现有 `MockProvider` 基建，见本文件既有测试如 `SubAgentRunner::new(provider)` 的写法）：

```rust
    #[tokio::test]
    async fn compaction_uses_compact_provider() {
        use crate::test_utils::MockProvider;
        use deepseeknova_core::{RunInput, Runner};
        use tokio_stream::StreamExt;

        // 主 provider 回复正常文本；compact provider 回复特征摘要文本
        let main = Arc::new(MockProvider::text("main-answer"));
        let compact = Arc::new(MockProvider::text("COMPACT-DIGEST"));

        let mut runner = SubAgentRunner::new(main)
            .with_compact_provider(compact.clone())
            .with_compaction_threshold(1); // 阈值 1 token → 必触发压缩
        runner.register(SubAgentConfig::new("t", "you are t"));
        let runner = runner.with_default("t");

        let mut stream = runner
            .run_stream(RunInput {
                prompt: "goal: do something with enough words to exceed one token".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        // compact provider 被调用过（MockProvider 记录调用次数；若无计数方法，
        // 以其它可观测方式断言，如 MockProvider::calls()——按 test_utils 实际 API 调整）
        assert!(compact.call_count() >= 1, "compact provider should be used");
    }
```

> 说明：`MockProvider` 的调用计数 API 以 `crates/deepseeknova-agent/src/test_utils.rs` 实际定义为准；若无计数能力，给 `MockProvider` 补一个 `call_count()`（AtomicUsize 自增于 `generate`/`stream`），属测试基建的最小扩展。`RunInput` 若无 `Default`，按其字段全量构造（见本文件其他测试的构造方式）。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p deepseeknova-agent compaction_uses_compact_provider`
Expected: 编译失败 —— `with_compact_provider` 不存在。

- [ ] **Step 3: 实现**

(a) `SubAgentRunner` 结构体加字段与 builder：

```rust
pub struct SubAgentRunner {
    provider: Arc<dyn Provider>,
    /// Provider used for history compaction; falls back to `provider`.
    compact_provider: Option<Arc<dyn Provider>>,
    sub_agents: HashMap<String, SubAgentConfig>,
    default_sub_agent: Option<String>,
    compaction_threshold_tokens: Option<u32>,
}
```

`new()` 中初始化 `compact_provider: None,`；builder 方法（`with_compaction_threshold` 之后）：

```rust
    /// Use a dedicated provider (e.g. the `compact` model pointer) for
    /// history compaction instead of the main provider.
    pub fn with_compact_provider(mut self, provider: Arc<dyn Provider>) -> Self {
        self.compact_provider = Some(provider);
        self
    }
```

(b) `run_stream` 中克隆并传入 loop（`let provider = Arc::clone(&self.provider);` 之后）：

```rust
        let compact_provider = self
            .compact_provider
            .as_ref()
            .map(Arc::clone)
            .unwrap_or_else(|| Arc::clone(&self.provider));
```

`run_sub_agent_loop(...)` 调用与签名各加一个参数 `compact_provider: Arc<dyn Provider>`（放在 `provider` 之后），压缩调用点改为：

```rust
                match compact_with_provider(compact_provider.as_ref(), &all_msgs).await {
```

(c) `compact_with_provider` 从 `generate` 改为流式收集（这样 MeteredProvider 能从流中计量 compact 用量；无流式实现的 provider 会走默认回退，行为不变）：

```rust
    let validated = deepseeknova_provider::ValidatedRequest::new(&summary_msgs, &[])
        .map_err(|v| anyhow::anyhow!("invariant violation in sub-agent summarize: {:?}", v))?;
    let mut stream = provider.stream(validated).await?;
    let mut out = String::new();
    while let Some(chunk) = stream.next().await {
        if let Chunk::TextDelta(t) = chunk? {
            out.push_str(&t);
        }
    }
    Ok(out)
```

（文件头部确认已有 `use deepseeknova_core::chunk::{Chunk, Usage};` 与 `use tokio_stream::StreamExt;`，缺则补。）

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p deepseeknova-agent sub_agent`
Expected: 新旧测试全部 PASS。

- [ ] **Step 5: Commit**

```bash
git add crates/deepseeknova-agent
git commit -m "feat(agent): SubAgentRunner 压缩支持独立 compact provider 并改流式计量"
```

---

### Task 6: runtime — 委派引擎接 Task provider

**Files:**
- Modify: `crates/deepseeknova-runtime/src/lib.rs`

- [ ] **Step 1: 写失败测试**

在 `crates/deepseeknova-runtime/src/lib.rs` 底部测试模块内追加（复用该模块既有的 Provider stub，名字以实际为准，下称 `StubProvider`）：

```rust
    #[tokio::test]
    async fn build_agent_with_task_provider_compiles_and_registers_delegate() {
        let mut config = Config::default();
        config.delegate.enabled = true;
        let main: Arc<dyn deepseeknova_provider::Provider> = Arc::new(StubProvider);
        let task: Arc<dyn deepseeknova_provider::Provider> = Arc::new(StubProvider);
        let agent = build_agent_with_task_provider(
            &config,
            std::env::temp_dir(),
            main,
            Some(task),
            0,
            None,
            vec![],
        )
        .unwrap();
        // 委派开启时 delegate 扩展被注入（与既有
        // build_agent_registers_delegate_tool_when_enabled 同样的断言方式）
        let _ = agent;
    }
```

（断言细节参照既有测试 `build_agent_registers_delegate_tool_when_enabled` 的写法，保持一致。）

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p deepseeknova-runtime build_agent_with_task_provider`
Expected: 编译失败 —— 函数不存在。

- [ ] **Step 3: 实现**

(a) 将现 `pub fn build_agent(...)` 重命名为内部实现并新增带 task provider 的公开入口，保持旧签名兼容：

```rust
/// Like [`build_agent`], but routes the delegate engine's sub-agents to a
/// dedicated `task` provider (the `task` model pointer). `None` falls back
/// to the main provider — identical to [`build_agent`].
#[allow(clippy::too_many_arguments)]
pub fn build_agent_with_task_provider(
    config: &Config,
    workspace_root: PathBuf,
    provider: Arc<dyn deepseeknova_provider::Provider>,
    task_provider: Option<Arc<dyn deepseeknova_provider::Provider>>,
    max_steps: usize,
    gate: Option<Arc<PermissionGate>>,
    extra_tools: Vec<Arc<dyn deepseeknova_core::Tool>>,
) -> anyhow::Result<deepseeknova_agent::Agent> {
    // …原 build_agent 函数体整体移入此处…
}

pub fn build_agent(
    config: &Config,
    workspace_root: PathBuf,
    provider: Arc<dyn deepseeknova_provider::Provider>,
    max_steps: usize,
    gate: Option<Arc<PermissionGate>>,
    extra_tools: Vec<Arc<dyn deepseeknova_core::Tool>>,
) -> anyhow::Result<deepseeknova_agent::Agent> {
    build_agent_with_task_provider(
        config,
        workspace_root,
        provider,
        None,
        max_steps,
        gate,
        extra_tools,
    )
}
```

(b) 函数体内委派引擎构建处（原 `build_delegate_engine(config, Arc::clone(&provider), ...)`）改为：

```rust
    if config.delegate.enabled {
        let delegate_provider = task_provider
            .as_ref()
            .map(Arc::clone)
            .unwrap_or_else(|| Arc::clone(&provider));
        let engine = build_delegate_engine(
            config,
            delegate_provider,
            &workspace_root,
            &security,
            gate.clone(),
            graph_ext.clone(),
            memory_ext.clone(),
        );
        let handle: deepseeknova_tools::DelegateHandle = engine;
        agent = agent.with_extension(handle);
    }
```

`build_delegate_engine` 本身签名不变（它已接收 provider 参数）。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p deepseeknova-runtime`
Expected: 全部 PASS（含既有 build_agent 测试，验证旧入口未破坏）。

- [ ] **Step 5: Commit**

```bash
git add crates/deepseeknova-runtime
git commit -m "feat(runtime): build_agent_with_task_provider 委派引擎路由 task 模型"
```

---

### Task 7: CLI main.rs — 构建 Router 并按角色装配

**Files:**
- Modify: `crates/deepseeknova-cli/src/main.rs`

- [ ] **Step 1: 构建 router + ledger**

`main()` 中 `Config::load()`（约 L22）之后追加：

```rust
    let cost_ledger = std::sync::Arc::new(deepseeknova_provider::cost::CostLedger::new());
    let model_router = std::sync::Arc::new(
        deepseeknova_provider::router::ModelRouter::from_config(
            &config,
            std::sync::Arc::clone(&cost_ledger),
        )
        .unwrap_or_else(|e| {
            eprintln!("config error: {e}");
            std::process::exit(2);
        }),
    );
```

- [ ] **Step 2: 两处 chat 分支的 agent_factory 改走 router**

`Commands::Chat`（约 L204）与 `None`（约 L337）两个 factory 闭包，闭包前加 `let router = Arc::clone(&model_router);`，闭包体改为：

```rust
                    move |effort: Option<deepseeknova_provider::factory::ReasoningEffort>,
                          model_name: Option<String>|
                          -> anyhow::Result<Box<dyn Runner + Send>> {
                        use deepseeknova_provider::cost::ModelRole;
                        let provider = match &model_name {
                            // `/model switch <name>` 显式覆盖，仍按 Main 角色计量
                            Some(m) => router.provider_for_model(m, ModelRole::Main, effort)?,
                            None => router.provider_for(ModelRole::Main, effort)?,
                        };
                        let task_provider = router.provider_for(ModelRole::Task, effort)?;
                        let agent = build_agent(
                            provider,
                            Some(task_provider),
                            model_name.as_deref(),
                            cfg,
                            0,
                            mcp_tools.clone(),
                        )?
                        .with_conversation_history(Arc::clone(&history_clone));
                        Ok(Box::new(agent))
                    };
```

- [ ] **Step 3: 本地 build_agent 包装签名扩展**

main.rs 底部本地 `fn build_agent(provider, _model, config, max_steps, extra_tools)` 增加第二参数 `task_provider: Option<Arc<dyn deepseeknova_provider::Provider>>`，内部改调 `deepseeknova_runtime::build_agent_with_task_provider(config, workspace_root, provider, task_provider, max_steps, gate, extra_tools)`（gate 等原有装配不动）。`Serve` 分支调用处（约 L244）补传 `None`。

同时 `run_chat_repl` 两处调用（约 L221、L354）各追加实参 `Some(Arc::clone(&model_router))`（对应 Task 8 的新形参）。

- [ ] **Step 4: 编译**

Run: `cargo check -p deepseeknova-cli`
Expected: 仅 chat.rs 缺参错误（Task 8 解决）；若此时把 Task 8 一并完成后再编译，直接绿。本任务与 Task 8 允许合并为一次提交前的连续工作，但提交拆开（见各自 commit）。

- [ ] **Step 5: Commit**（在 Task 8 编译通过后执行）

```bash
git add crates/deepseeknova-cli/src/main.rs
git commit -m "feat(cli): 主/任务角色经 ModelRouter 装配 agent"
```

---

### Task 8: CLI chat.rs — /model 指针显示 + /model use + /cost

**Files:**
- Modify: `crates/deepseeknova-cli/src/chat.rs`

- [ ] **Step 1: 签名与透传**

`run_chat_repl` 增加末位参数 `router: Option<std::sync::Arc<deepseeknova_provider::router::ModelRouter>>`；函数内把 `router.as_ref()` 透传给 `handle_slash_command`（同样加末位参数 `router: Option<&std::sync::Arc<...ModelRouter>>`）。

- [ ] **Step 2: `/model` 帮助与指针显示**

`"model"` 分支 `"" | "help"` 打印段（约 L374）追加：

```rust
                    println!("  /model use <role> <name>  — set a role pointer: main|task|compact|quick");
                    if let Some(r) = router {
                        use deepseeknova_provider::cost::ModelRole;
                        println!();
                        println!("Model pointers:");
                        for role in [ModelRole::Main, ModelRole::Task, ModelRole::Compact, ModelRole::Quick] {
                            println!(
                                "  {:<8} → {}",
                                role.label(),
                                r.pointer(role).unwrap_or_else(|| "(default)".to_string())
                            );
                        }
                    }
```

- [ ] **Step 3: `/model use <role> <model>` 子命令**

`"switch"` 分支之后新增：

```rust
                "use" => {
                    let mut parts = sub_args.split_whitespace();
                    match (parts.next(), parts.next(), router) {
                        (Some(role_s), Some(model), Some(r)) => {
                            match deepseeknova_provider::cost::ModelRole::parse(role_s) {
                                Some(role) => match r.set_pointer(role, model) {
                                    Ok(()) => {
                                        println!("pointer {} → {model}", role.label());
                                        // 重建 agent 使新指针生效（含委派引擎）
                                        return Ok(SlashAction::Rebuild {
                                            effort: None,
                                            model: None,
                                        });
                                    }
                                    Err(e) => eprintln!("{e}"),
                                },
                                None => eprintln!("unknown role '{role_s}': main|task|compact|quick"),
                            }
                        }
                        (_, _, None) => eprintln!("model pointers unavailable (no router)"),
                        _ => eprintln!("Usage: /model use <main|task|compact|quick> <model-name>"),
                    }
                }
```

- [ ] **Step 4: `/cost` 命令**

在斜杠命令 match 中（`"model"` 分支之后）新增：

```rust
        // ── Cost accounting ───────────────────────────────────
        "cost" => {
            match router {
                Some(r) => {
                    let report = r.ledger().report(&r.price_table());
                    if report.rows.is_empty() {
                        println!("no usage recorded yet");
                    } else {
                        println!(
                            "{:<24} {:<8} {:>10} {:>12} {:>10} {:>10}",
                            "model", "role", "prompt", "completion", "cache-hit", "cost($)"
                        );
                        for row in &report.rows {
                            println!(
                                "{:<24} {:<8} {:>10} {:>12} {:>10} {:>10}",
                                row.model,
                                row.role.label(),
                                row.bucket.prompt_tokens,
                                row.bucket.completion_tokens,
                                row.bucket.cache_hit_tokens,
                                row.cost_usd
                                    .map(|c| format!("{c:.4}"))
                                    .unwrap_or_else(|| "-".to_string()),
                            );
                        }
                        if let Some(total) = report.total_usd {
                            println!("total estimated: ${total:.4}");
                        }
                        if report.unmetered_calls > 0 {
                            println!("note: {} call(s) had no usage info (not estimated)", report.unmetered_calls);
                        }
                    }
                }
                None => println!("cost accounting unavailable (no router)"),
            }
        }
```

并在 `/help` 输出与 REPL 顶栏提示中补 `/cost`。

- [ ] **Step 5: 编译与手工冒烟**

Run: `cargo check -p deepseeknova-cli && cargo test -p deepseeknova-cli`
Expected: 编译通过、既有测试 PASS。
手工冒烟（需 `DEEPSEEK_API_KEY`，无 key 时跳过并在 PR 说明）：`cargo run -p deepseeknova-cli -- chat`，输入 `/model`（应显示四指针）、`/cost`（应显示 no usage 或计量表）、`/model use quick <某已配置模型>`。

- [ ] **Step 6: Commit**

```bash
git add crates/deepseeknova-cli/src/chat.rs
git commit -m "feat(cli): /model 指针管理与 /cost 成本报表"
```

（随后补 Task 7 的 main.rs 提交，若尚未提交。）

---

### Task 9: 文档与全量回归

**Files:**
- Modify: `GUIDE.md`（配置章节）

- [ ] **Step 1: GUIDE.md 配置章节追加**

在 GUIDE.md 配置示例区（`[[models]]` 相关段落附近）追加：

```markdown
### 模型指针与成本分账

按角色路由模型（均可选；未配置的角色回落 `main`，`main` 未配置则用默认 provider）：

​```toml
[model_pointers]
main = "deepseek-v4"          # 主对话
task = "deepseek-v4-flash"    # 子代理/委派
compact = "deepseek-v4-flash" # 历史压缩
quick = "deepseek-v4-flash"   # 快速操作

[[models]]
name = "deepseek-v4"
provider = "deepseek"
input_price_per_mtok = 0.28    # $/1M tokens，可选；配齐 input+output 才输出美元估算
output_price_per_mtok = 0.42
cache_hit_price_per_mtok = 0.028
​```

会话内：`/model` 查看指针，`/model use <role> <model>` 热切换（不写盘），`/cost` 查看
按 模型×角色 的 token 用量与成本估算。
```

（写入时去掉代码块前的零宽转义。）

- [ ] **Step 2: 全量回归**

Run: `make check`
Expected: fmt + clippy + test + doc 全绿（跨 crate 变更强制项）。若 clippy 对新代码报警，按警告修复后重跑。

- [ ] **Step 3: Commit**

```bash
git add GUIDE.md
git commit -m "docs(guide): 模型指针与成本分账配置说明"
```

---

## 自检与假设

- **Spec 覆盖对照**：配置层（Task 1）、Provider 层 router/cost（Task 2–4）、Agent/CLI 接线（Task 5–8）、错误边界（Task 1 校验 / Task 3 unmetered / Task 4 set_pointer 报错）、测试计划（各任务 TDD + Task 9 make check）——spec 全部小节均有对应任务。
- **已知不确定点（实现时按实际微调，不改架构）**：
  1. `MockProvider` 是否有调用计数——无则按 Task 5 说明补最小 `call_count()`；
  2. runtime 测试 stub 名称与 `RunInput` 构造方式——以现文件为准；
  3. provider crate `Cargo.toml` 是否已有 `toml`/`tokio`（tokio::test 用）dev-dependency——缺则补 workspace 依赖。
- **回滚**：每任务独立提交，任一任务失败可 revert 单个 commit 而不影响其余。
