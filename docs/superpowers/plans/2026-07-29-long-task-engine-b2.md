# Long-Task Engine B2 — 续航（L3 压缩 · 会话 v2 · budget · pause）· Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 agent 能跑超长任务不断链：会话持久化保真（tool_calls/reasoning 不丢、resume 后过 replay 校验）、上下文超限时 L3 结构化摘要压缩（带熔断回退）、budget 在 step 边界守门、max_steps 到顶默认优雅暂停（`Paused` 事件 + CLI exit 3，逃生舱 `"error"`）。

**Architecture:** 方案 A 延续——零新 crate，全部为现有组件的接线与增量。四个"已建未接"组件被接入主循环：`PromptBudgetController`（零调用→step 边界评估）、`group_into_units`/`AtomicUnitCompactor`/`Memory::compact`（→L3 通路）、`SessionStore`（有损→schema v2 保真）、`RunEvent`（新增 `Paused` 变体贯通 core→agent→CLI/Wire）。前缀缓存约束：所有改动只触碰 volatile 区之后的历史，system prefix 字节不变。

**Tech Stack:** Rust（tokio/serde/anyhow+thiserror），既有 crate：core/agent/store/config/cli/runtime/context。

**基线：** worktree `.worktrees/feat-long-task-b2` @ `9cb18c1`（=origin/main），`cargo test --workspace --exclude deepseeknova-desktop` 全绿（50 目标）。

---

## File Map（本计划全部触点）

| 文件 | 动作 | 职责 |
|---|---|---|
| `crates/deepseeknova-config/src/lib.rs` | Modify | 新增 `SessionConfig`/`BudgetConfig` 段 + `AgentConfig.{on_max_steps,l3_compaction,compact_model}` |
| `crates/deepseeknova-store/src/lib.rs` | Modify | `StoredMessage` schema v2（tool_calls/reasoning_content，serde(default) 兼容旧文件） |
| `crates/deepseeknova-core/src/runner.rs` | Modify | `RunEvent::Paused` + `WireEvent::Paused` + From 臂 |
| `crates/deepseeknova-agent/src/compaction.rs` | Create | L3 结构化摘要（7 段 prompt + 状态重建辅助），pub(crate) |
| `crates/deepseeknova-agent/src/agent.rs` | Modify | budget 接线、L3 触发+熔断、on_max_steps 分支、builder |
| `crates/deepseeknova-agent/src/lib.rs` | Modify | `mod compaction;` |
| `crates/deepseeknova-runtime/src/lib.rs` | Modify | `build_agent` 装配 on_max_steps/l3/compact_model/budget |
| `crates/deepseeknova-cli/src/main.rs` | Modify | `stream_events` 处理 `Paused` → resume 提示 + exit code 3 |
| `crates/deepseeknova-store/tests/resume_fidelity.rs` | Create | 保真回程 + 旧格式兼容 + replay 校验集成测试 |
| `CHANGELOG.md` | Modify | `on_max_steps` breaking change 标注 |

**穷尽匹配审计（写码前已核）**：`RunEvent` 唯一穷尽 match 是 `runner.rs` 的 `From<RunEvent> for WireEvent`；CLI 的 `stream_events`/`stream_coordinator` 均有 `_ => {}` 兜底；desktop/serve/tui 消费 `WireEvent`（tagged serde，新增 kind 对旧前端是未知项，不 panic）。

**偏差记录（相对 spec 措辞，已按现状裁定）**：
1. spec 的 `[session] root=".deepseeknova/sessions"` → 现状 CLI 已用 `~/.deepseeknova/sessions`（`sessions_root()`）。裁定：`root=""` 默认表示沿用现状 home 路径，非空则作为显式路径。零行为变化。
2. spec "压缩后注入最近改动文件路径清单" → 实现为从被驱逐消息的 write/edit 类 tool_calls 参数中提取 `path` 字段（尽力而为，解析失败即跳过），不引入新的全局跟踪状态。
3. `RunEvent::Paused.session_id` 由 builder 注入（`with_session_label`）：Agent 不感知存储，CLI/desktop 在启用持久化时把 session id 标注给 Agent，仅用于提示文案。

---

## Task 1: config — `[session]`/`[budget]` 段 + AgentConfig 三字段

**Files:**
- Modify: `crates/deepseeknova-config/src/lib.rs`

- [ ] **Step 1: 写失败测试** — 在 `mod tests` 内追加：

```rust
    #[test]
    fn session_budget_config_defaults() {
        let c = Config::default();
        assert!(c.session.enabled);
        assert_eq!(c.session.root, "");
        assert!(c.budget.enabled);
        assert_eq!(c.budget.max_total_tokens, 128_000);
        assert_eq!(c.budget.max_memory_tokens, 32_000);
        assert_eq!(c.agent.on_max_steps, "pause");
        assert!(c.agent.l3_compaction);
        assert_eq!(c.agent.compact_model, "");
    }

    #[test]
    fn agent_b2_fields_parse_overrides() {
        let toml = "[agent]\non_max_steps = \"error\"\nl3_compaction = false\ncompact_model = \"deepseek-chat\"\n\n[budget]\nenabled = false\nmax_total_tokens = 64000\n";
        let c: Config = toml::from_str(toml).unwrap();
        assert_eq!(c.agent.on_max_steps, "error");
        assert!(!c.agent.l3_compaction);
        assert_eq!(c.agent.compact_model, "deepseek-chat");
        assert!(!c.budget.enabled);
        assert_eq!(c.budget.max_total_tokens, 64_000);
        assert_eq!(c.budget.max_memory_tokens, 32_000); // 未覆盖取默认
        assert!(c.session.enabled); // 未写 [session] 取默认
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p deepseeknova-config --lib session_budget 2>&1 | tail -3`
Expected: 编译错误 `no field 'session' on type 'Config'`

- [ ] **Step 3: 实现** — 在 `Config` struct（`pub telemetry: TelemetryConfig,` 之后）追加：

```rust
    /// Session persistence for long-task resume (B2).
    #[serde(default)]
    pub session: SessionConfig,

    /// Prompt budget guard evaluated at agent step boundaries (B2).
    #[serde(default)]
    pub budget: BudgetConfig,
```

在 Telemetry 段之后新增两个段（对齐 `TelemetryConfig` 的排版风格）：

```rust
// ---------------------------------------------------------------------------
// Session（长任务会话持久化）
// ---------------------------------------------------------------------------

/// Session persistence configuration (long-task resume).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    /// Whether chat/run sessions are persisted (default: true).
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Session store root. Empty (default) = `~/.deepseeknova/sessions`
    /// (the pre-B2 behavior); non-empty = explicit directory path.
    #[serde(default)]
    pub root: String,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            root: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Budget（step 边界上下文预算守门）
// ---------------------------------------------------------------------------

/// Prompt budget configuration, feeding `PromptBudgetController`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    /// Whether the budget guard runs at step boundaries (default: true).
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Hard context ceiling in estimated tokens (default: 128000).
    #[serde(default = "default_budget_total")]
    pub max_total_tokens: usize,

    /// Memory sub-budget in estimated tokens (default: 32000).
    #[serde(default = "default_budget_memory")]
    pub max_memory_tokens: usize,
}

fn default_budget_total() -> usize {
    128_000
}

fn default_budget_memory() -> usize {
    32_000
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_total_tokens: default_budget_total(),
            max_memory_tokens: default_budget_memory(),
        }
    }
}
```

在 `AgentConfig`（`plan_mode_default` 字段之后）追加三个字段：

```rust
    /// What to do when max_steps is exhausted: "pause" (default, saves the
    /// session and emits RunEvent::Paused) or "error" (pre-B2 behavior).
    #[serde(default = "default_on_max_steps")]
    pub on_max_steps: String,

    /// Enable L3 structured LLM compaction. false = L1/L2 only (pre-B2).
    #[serde(default = "default_true")]
    pub l3_compaction: bool,

    /// Model used for L3 compaction digests. Empty = main model.
    #[serde(default)]
    pub compact_model: String,
```

配套：

```rust
fn default_on_max_steps() -> String {
    "pause".to_string()
}
```

`impl Default for AgentConfig` 的 `Self { ... }` 内补：

```rust
            on_max_steps: default_on_max_steps(),
            l3_compaction: true,
            compact_model: String::new(),
```

`impl AgentConfig::merge` 末尾补（跟随既有 max_steps/concurrent_tools 的覆盖风格）：

```rust
        self.on_max_steps = other.on_max_steps;
        self.l3_compaction = other.l3_compaction;
        if !other.compact_model.is_empty() {
            self.compact_model = other.compact_model;
        }
```

`impl Config::merge` 内（`self.telemetry.merge(other.telemetry);` 之后）补：

```rust
        self.session = other.session;
        self.budget = other.budget;
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p deepseeknova-config 2>&1 | grep -E 'test result'`
Expected: 全部 `ok`（含既有测试无回归）

- [ ] **Step 5: fmt + 提交**

```bash
cargo fmt -p deepseeknova-config
git add crates/deepseeknova-config/src/lib.rs
git commit -m "feat(config): [session]/[budget] sections + agent on_max_steps/l3_compaction/compact_model"
```

---

## Task 2: store — StoredMessage schema v2（保真持久化）

**Files:**
- Modify: `crates/deepseeknova-store/src/lib.rs`

- [ ] **Step 1: 确认 ToolCall 可序列化**

Run: `grep -n -B2 'pub struct ToolCall' crates/deepseeknova-core/src/types.rs | head -5`
Expected: derive 列表含 `Serialize, Deserialize`。若不含：给 `ToolCall`/`FunctionCall` 补 derive（它们本就是 wire 类型，补全安全），并在提交信息注明。

- [ ] **Step 2: 写失败测试** — 在 `mod tests` 内追加：

```rust
    #[test]
    fn stored_message_roundtrips_tool_calls_and_reasoning() {
        use deepseeknova_core::types::{FunctionCall, ToolCall};
        let msg = Message {
            role: Role::Assistant,
            content: String::new(),
            name: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: "{\"path\":\"src/lib.rs\"}".into(),
                },
            }]),
            tool_call_id: None,
            reasoning_content: Some("I should read the file first.".into()),
        };
        let turn = SessionStore::build_turn(&sample_input(), 1, vec![msg], None);
        let json = serde_json::to_string(&turn).unwrap();
        let parsed: StoredTurn = serde_json::from_str(&json).unwrap();
        let restored: Message = (&parsed.messages[0]).into();
        let tcs = restored.tool_calls.expect("tool_calls must survive");
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].id, "call_1");
        assert_eq!(tcs[0].function.name, "read_file");
        assert_eq!(
            restored.reasoning_content.as_deref(),
            Some("I should read the file first.")
        );
    }

    #[test]
    fn legacy_stored_message_without_new_fields_still_parses() {
        // 旧版 JSONL 行（无 tool_calls/reasoning_content 字段）必须照常反序列化。
        let legacy = "{\"role\":\"user\",\"content\":\"hi\"}";
        let sm: StoredMessage = serde_json::from_str(legacy).unwrap();
        assert!(sm.tool_calls.is_none());
        assert!(sm.reasoning_content.is_none());
        let m: Message = (&sm).into();
        assert!(m.tool_calls.is_none());
    }
```

> 注：`ToolCall` 字段名以 Step 1 实际输出为准；若 struct 字段与上述（`id`/`call_type`/`function{name,arguments}`）不一致，按真实定义调整测试构造，语义不变。

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p deepseeknova-store --lib stored_message 2>&1 | tail -3`
Expected: 编译错误 `no field 'tool_calls' on type StoredMessage`

- [ ] **Step 4: 实现三处**

(a) `StoredMessage` struct 末尾（`tool_call_id` 之后）追加：

```rust
    /// Assistant tool calls (schema v2). `serde(default)` keeps old files readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<deepseeknova_core::types::ToolCall>>,
    /// DeepSeek-V4 reasoning content (schema v2), required for replay fidelity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
```

(b) `build_turn` 的消息映射改为：

```rust
                .map(|m| StoredMessage {
                    role: role_to_str(&m.role),
                    content: m.content,
                    name: m.name,
                    tool_call_id: m.tool_call_id,
                    tool_calls: m.tool_calls,
                    reasoning_content: m.reasoning_content,
                })
```

(c) `impl From<&StoredMessage> for Message` 中把两处 `None` 硬编码改为：

```rust
            tool_calls: sm.tool_calls.clone(),
            reasoning_content: sm.reasoning_content.clone(),
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p deepseeknova-store 2>&1 | grep -E 'test result'`
Expected: 全 `ok`（含既有 roundtrip/resume 测试无回归——它们即是"旧行为不破"的守卫）

- [ ] **Step 6: fmt + 提交**

```bash
cargo fmt -p deepseeknova-store
git add crates/deepseeknova-store/src/lib.rs
git commit -m "feat(store): schema v2 — persist tool_calls + reasoning_content (serde-default compat)"
```


---

## Task 3: core — `RunEvent::Paused` + `WireEvent::Paused`

**Files:**
- Modify: `crates/deepseeknova-core/src/runner.rs`

- [ ] **Step 1: 写失败测试** — 在 runner.rs 的 `mod tests`（若无则在文件末尾新建 `#[cfg(test)] mod tests { use super::*; }`）追加：

```rust
    #[test]
    fn paused_event_maps_to_wire() {
        let ev = RunEvent::Paused {
            reason: "reached max steps (10)".into(),
            session_id: Some("chat-20260729-120000".into()),
        };
        let wire: WireEvent = ev.into();
        match wire {
            WireEvent::Paused { reason, session_id } => {
                assert_eq!(reason, "reached max steps (10)");
                assert_eq!(session_id.as_deref(), Some("chat-20260729-120000"));
            }
            other => panic!("expected Paused, got {other:?}"),
        }
    }

    #[test]
    fn paused_wire_event_serializes_with_kind_tag() {
        let wire = WireEvent::Paused {
            reason: "budget: over limit".into(),
            session_id: None,
        };
        let json = serde_json::to_string(&wire).unwrap();
        assert!(json.contains("\"kind\":\"paused\""), "json = {json}");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p deepseeknova-core --lib paused 2>&1 | tail -3`
Expected: 编译错误 `no variant named 'Paused'`

- [ ] **Step 3: 实现三处**

(a) `RunEvent` 枚举（`Done(RunOutput),` 之前）追加：

```rust
    /// The run stopped gracefully before completion (max-steps pause or
    /// budget rejection). The task is resumable: frontends should surface
    /// `reason` and, when present, which saved session to resume.
    Paused {
        reason: String,
        session_id: Option<String>,
    },
```

(b) `WireEvent` 枚举（`Error { message: String },` 之前）追加：

```rust
    Paused {
        reason: String,
        session_id: Option<String>,
    },
```

(c) `impl From<RunEvent> for WireEvent` 的 match 中追加一臂：

```rust
            RunEvent::Paused { reason, session_id } => WireEvent::Paused { reason, session_id },
```

- [ ] **Step 4: 全工作区编译核查（穷尽匹配无遗漏）**

Run: `cargo check --workspace --exclude deepseeknova-desktop 2>&1 | tail -3 && cargo test -p deepseeknova-core --lib paused 2>&1 | grep 'test result'`
Expected: check 无错（消费端均有 `_ =>` 兜底）；2 个测试 `ok`

- [ ] **Step 5: fmt + 提交**

```bash
cargo fmt -p deepseeknova-core
git add crates/deepseeknova-core/src/runner.rs
git commit -m "feat(core): RunEvent::Paused + wire mapping for graceful long-task pause"
```

---

## Task 4: agent — `compaction.rs` L3 结构化压缩模块

**Files:**
- Create: `crates/deepseeknova-agent/src/compaction.rs`
- Modify: `crates/deepseeknova-agent/src/lib.rs`（加 `mod compaction;`，crate 内部可见即可）

设计：纯逻辑（渲染/提取/熔断）与 LLM 调用分离，纯逻辑全部单测；LLM 通路在 Task 5 的 agent 集成测试覆盖。**必守约束**：`memory.has_pending_must_replay()` 为真时 L3 直接顺延（不计失败——那是正确性保护不是故障）；LLM 失败计一败，**连败 3 次本会话熔断**（`disabled`），回退 L2-only 现状。

- [ ] **Step 1: 导出模块** — `crates/deepseeknova-agent/src/lib.rs` 在既有 `mod` 声明区（字母序）加：

```rust
mod compaction;
```

- [ ] **Step 2: 建文件（实现+测试一体）** — Create `crates/deepseeknova-agent/src/compaction.rs`：

```rust
//! L3 结构化压缩（B2）：L1/L2 之后仍超阈值时，将可安全驱逐的历史交给
//! （可配置的廉价）模型产出 7 段结构化摘要，经 `Memory::compact` 落回，
//! 并做压缩后状态重建：注入最近改动文件路径清单（仅路径）+ 重放最后一条
//! 用户消息。失败回退 L2-only 现状；连败 3 次本会话熔断。
//!
//! 前缀缓存约束：本模块只改写 volatile 区之后的历史（`Memory` 内容），
//! 绝不触碰 system prefix。

use crate::memory::Memory;
use deepseeknova_core::{Message, Role};
use deepseeknova_context::history::group_into_units;
use deepseeknova_provider::Provider;
use tracing::{info, warn};

/// 连败多少次后本会话停用 L3（Claude Code 同款保险）。
const MAX_STRIKES: u32 = 3;

/// 会话级 L3 压缩器：持有熔断状态。
pub(crate) struct L3Compactor {
    failures: u32,
    disabled: bool,
}

impl L3Compactor {
    pub(crate) fn new() -> Self {
        Self {
            failures: 0,
            disabled: false,
        }
    }

    pub(crate) fn is_disabled(&self) -> bool {
        self.disabled
    }

    fn record_failure(&mut self) {
        self.failures += 1;
        if self.failures >= MAX_STRIKES {
            self.disabled = true;
            warn!("L3 compaction disabled for this session after {MAX_STRIKES} consecutive failures");
        }
    }

    fn record_success(&mut self) {
        self.failures = 0;
    }

    /// 尝试一次 L3 压缩。返回 true = 已压缩落回；false = 未压缩
    /// （must_replay 顺延 / 已熔断 / LLM 失败），调用方保持 L2-only 现状。
    pub(crate) async fn try_compact(
        &mut self,
        provider: &dyn Provider,
        memory: &mut Memory,
    ) -> bool {
        if self.disabled {
            return false;
        }
        // 正确性保护：存在未消费的 must_replay 推理块时顺延，不计失败。
        if memory.has_pending_must_replay() {
            info!("L3 deferred: pending must_replay reasoning blocks");
            return false;
        }

        let all_msgs = memory.get_all();
        let last_user = last_user_message(&all_msgs);
        let touched = extract_touched_files(&all_msgs);
        let prompt = render_l3_prompt(&all_msgs);

        match summarize(provider, &prompt).await {
            Ok(digest) => {
                memory.compact(digest, None);
                // 状态重建①：最近改动文件路径清单（仅路径，非内容）。
                if !touched.is_empty() {
                    memory.add_message(Message {
                        role: Role::User,
                        content: format!(
                            "[Recently touched files]\n{}",
                            touched.join("\n")
                        ),
                        name: None,
                        tool_calls: None,
                        tool_call_id: None,
                        reasoning_content: None,
                    });
                }
                // 状态重建②：重放最后一条用户消息，让任务从原意图继续。
                if let Some(u) = last_user {
                    memory.add_message(u);
                }
                self.record_success();
                true
            }
            Err(e) => {
                warn!("L3 compaction failed ({e}); falling back to L2-only");
                self.record_failure();
                false
            }
        }
    }
}

/// 渲染 7 段结构化摘要 prompt（要求直引原文关键短语防漂移）。
fn render_l3_prompt(messages: &[Message]) -> String {
    // 按压缩安全单元渲染，tool 交换以紧凑形式呈现。
    let units = group_into_units(messages);
    let mut convo = String::new();
    for u in &units {
        convo.push_str(&render_unit(u));
        convo.push('\n');
    }
    format!(
        "You are compacting an agent conversation into a structured digest. \
         Produce EXACTLY these seven sections, each as a markdown heading, \
         quoting key phrases verbatim from the source to avoid drift:\n\
         ## Original intent\n## Key decisions\n## Files involved\n\
         ## Errors & fixes\n## TODOs\n## In progress\n## Next step\n\n\
         Conversation:\n{convo}"
    )
}

fn render_unit(u: &deepseeknova_context::history::HistoryUnit) -> String {
    use deepseeknova_context::history::HistoryUnit;
    match u {
        HistoryUnit::Standalone(m) => format!("[{:?}] {}", m.role, m.content),
        HistoryUnit::ToolExchange { assistant, results } => {
            let calls: Vec<String> = assistant
                .tool_calls
                .iter()
                .flatten()
                .map(|tc| format!("{}({})", tc.function.name, tc.function.arguments))
                .collect();
            let outs: Vec<String> = results
                .iter()
                .map(|r| truncate(&r.content, 400))
                .collect();
            format!("[ToolExchange] calls: {} | results: {}", calls.join("; "), outs.join(" | "))
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}…")
    }
}

/// 从被压缩历史的 write/edit 类工具调用参数中尽力提取 `path` 字段。
/// 解析失败一律静默跳过——这是提示性重建，不是事实源。
fn extract_touched_files(messages: &[Message]) -> Vec<String> {
    const WRITE_TOOLS: [&str; 4] = ["write_file", "edit_file", "apply_patch", "create_file"];
    let mut seen = std::collections::BTreeSet::new();
    for m in messages {
        for tc in m.tool_calls.iter().flatten() {
            if !WRITE_TOOLS.contains(&tc.function.name.as_str()) {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&tc.function.arguments) {
                if let Some(p) = v.get("path").and_then(|p| p.as_str()) {
                    seen.insert(p.to_string());
                }
            }
        }
    }
    seen.into_iter().collect()
}

fn last_user_message(messages: &[Message]) -> Option<Message> {
    messages.iter().rev().find(|m| m.role == Role::User).cloned()
}

/// 单次 LLM 摘要调用：走 Provider 非流式生成，取文本。
async fn summarize(provider: &dyn Provider, prompt: &str) -> anyhow::Result<String> {
    let msgs = vec![Message {
        role: Role::User,
        content: prompt.to_string(),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    }];
    let out = provider.generate(&msgs, &[]).await?;
    if out.text.trim().is_empty() {
        anyhow::bail!("empty digest from compact model");
    }
    Ok(out.text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_core::types::{FunctionCall, ToolCall};

    fn msg(role: Role, content: &str) -> Message {
        Message {
            role,
            content: content.into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    #[test]
    fn prompt_renders_seven_sections() {
        let p = render_l3_prompt(&[msg(Role::User, "fix the bug in auth")]);
        for h in [
            "## Original intent",
            "## Key decisions",
            "## Files involved",
            "## Errors & fixes",
            "## TODOs",
            "## In progress",
            "## Next step",
        ] {
            assert!(p.contains(h), "missing section {h}");
        }
        assert!(p.contains("fix the bug in auth"));
    }

    #[test]
    fn extracts_paths_only_from_write_tools() {
        let mut m = msg(Role::Assistant, "");
        m.tool_calls = Some(vec![
            ToolCall {
                id: "1".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "write_file".into(),
                    arguments: "{\"path\":\"src/a.rs\",\"content\":\"x\"}".into(),
                },
            },
            ToolCall {
                id: "2".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: "{\"path\":\"src/b.rs\"}".into(),
                },
            },
            ToolCall {
                id: "3".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "edit_file".into(),
                    arguments: "not-json".into(),
                },
            },
        ]);
        let files = extract_touched_files(&[m]);
        assert_eq!(files, vec!["src/a.rs".to_string()]); // read 不算、坏 JSON 跳过
    }

    #[test]
    fn strike_counter_disables_after_three() {
        let mut c = L3Compactor::new();
        assert!(!c.is_disabled());
        c.record_failure();
        c.record_failure();
        assert!(!c.is_disabled());
        c.record_failure();
        assert!(c.is_disabled());
    }

    #[test]
    fn success_resets_strikes() {
        let mut c = L3Compactor::new();
        c.record_failure();
        c.record_failure();
        c.record_success();
        c.record_failure();
        assert!(!c.is_disabled(), "success must reset the strike counter");
    }

    #[test]
    fn last_user_message_picks_most_recent() {
        let msgs = vec![
            msg(Role::User, "first"),
            msg(Role::Assistant, "reply"),
            msg(Role::User, "second"),
        ];
        assert_eq!(last_user_message(&msgs).unwrap().content, "second");
    }
}
```

> **两个待核对点（实现者在写码时确认，不改语义）**：
> ① `Provider::generate` 的精确签名——以 `crates/deepseeknova-provider/src/lib.rs` 的 trait 定义为准调整 `summarize()` 的调用行与返回字段（取"最终文本"字段）。
> ② `ToolCall`/`FunctionCall` 字段名——以 `crates/deepseeknova-core/src/types.rs` 为准（Task 2 Step 1 已核过一次）。
> ③ `group_into_units`/`HistoryUnit` 的导出路径——若 `deepseeknova_context::history` 非 pub 路径，用该 crate 实际的 re-export（`deepseeknova_context::...`）。

- [ ] **Step 3: 跑测试确认通过**

Run: `cargo test -p deepseeknova-agent --lib compaction 2>&1 | grep 'test result'`
Expected: `5 passed; 0 failed`

- [ ] **Step 4: 核对 agent 是否已依赖 deepseeknova-context**

Run: `grep -n 'deepseeknova-context' crates/deepseeknova-agent/Cargo.toml || echo MISSING`
若 MISSING：在 `[dependencies]` 加 `deepseeknova-context = { workspace = true }`（workspace 根已声明）。

- [ ] **Step 5: fmt + clippy + 提交**

```bash
cargo fmt -p deepseeknova-agent
cargo clippy -p deepseeknova-agent --lib -- -D warnings
git add crates/deepseeknova-agent/src/compaction.rs crates/deepseeknova-agent/src/lib.rs crates/deepseeknova-agent/Cargo.toml
git commit -m "feat(agent): L3 structured compaction module (7-section digest, state rebuild, 3-strike breaker)"
```

---

## Task 5: agent — 主循环接线（budget 守门 + L3 触发 + on_max_steps 分支）

**Files:**
- Modify: `crates/deepseeknova-agent/src/agent.rs`
- Modify: `crates/deepseeknova-agent/src/lib.rs`（若 budget 模块非 pub，改为 `pub mod budget;` 以便 runtime 构造 `PromptBudgetController`）

- [ ] **Step 1: Agent 加字段与 builder** — `Agent` struct（`distill_hook` 字段之后）追加：

```rust
    /// max_steps 到顶行为：true = 优雅暂停（默认），false = 旧版报错。
    pause_on_max_steps: bool,

    /// L3 结构化压缩开关（config.agent.l3_compaction）。
    l3_enabled: bool,

    /// L3 摘要用 provider；None = 复用主 provider。
    compact_provider: Option<Arc<dyn Provider>>,

    /// step 边界预算守门；None = 关闭。
    budget: Option<crate::budget::controller::PromptBudgetController>,

    /// 暂停事件附带的会话标注（CLI/desktop 持久化开启时注入）。
    session_label: Option<String>,
```

构造处（`Agent::new` 的 `Self { ... }`）补对应默认值：`pause_on_max_steps: true, l3_enabled: true, compact_provider: None, budget: None, session_label: None,`

在既有 builder 方法区（`with_distill_hook` 之后）追加：

```rust
    /// 配置 max_steps 到顶行为："pause"（默认）或 "error"（旧行为逃生舱）。
    pub fn with_on_max_steps(mut self, mode: &str) -> Self {
        self.pause_on_max_steps = mode != "error";
        self
    }

    /// 开关 L3 结构化压缩（false = 仅 L1/L2 现状）。
    pub fn with_l3_compaction(mut self, enabled: bool) -> Self {
        self.l3_enabled = enabled;
        self
    }

    /// 指定 L3 摘要用的（廉价）provider；不设则复用主 provider。
    pub fn with_compact_provider(mut self, p: Arc<dyn Provider>) -> Self {
        self.compact_provider = Some(p);
        self
    }

    /// 启用 step 边界预算守门。
    pub fn with_budget(
        mut self,
        b: crate::budget::controller::PromptBudgetController,
    ) -> Self {
        self.budget = Some(b);
        self
    }

    /// 标注当前持久化会话 id（Paused 事件透出给前端）。
    pub fn with_session_label(mut self, id: impl Into<String>) -> Self {
        self.session_label = Some(id.into());
        self
    }
```

- [ ] **Step 2: 透传给 run_agent_loop** — `run_agent_loop` 签名追加 5 个参数（跟随现有参数排版）：

```rust
    pause_on_max_steps: bool,
    l3_enabled: bool,
    compact_provider: Option<Arc<dyn Provider>>,
    budget: Option<crate::budget::controller::PromptBudgetController>,
    session_label: Option<String>,
```

`run_stream` 内的调用点同步传入（从 `self.*` clone/copy；`Arc`/`Option<Arc>` 用 `clone()`）。

- [ ] **Step 3: 循环体三处改动**

(a) **budget 守门**——插在 `info!("agent step ...")` 之后、既有 compaction 块之前：

```rust
        // B2 预算守门：step 边界评估。CompressHistory 由下方压缩链处理；
        // Reject 时优雅暂停（保留历史写回路径），不再盲目上摊上下文。
        let mut budget_wants_compress = false;
        if let Some(ref b) = budget {
            const EXPECTED_TURN_TOKENS: usize = 2048; // 一轮回复的保守预估
            let current = estimate_tokens(&memory.get_all()) as usize;
            use crate::budget::controller::BudgetDecision;
            match b.evaluate_budget(current, EXPECTED_TURN_TOKENS) {
                BudgetDecision::Allow => {}
                BudgetDecision::CompressHistory => budget_wants_compress = true,
                BudgetDecision::Reject(why) => {
                    warn!("budget rejected further work: {why}");
                    tx.send(Ok(RunEvent::Paused {
                        reason: format!("budget: {why}"),
                        session_id: session_label.clone(),
                    }))
                    .await
                    .ok();
                    return Ok(());
                }
            }
        }
```

(b) **压缩链扩展**——把既有 `if let Some(threshold) = compaction_threshold { ... }` 块整体替换为（保持 L1/L2 原逻辑逐字不动，只是外层触发条件加 `|| budget_wants_compress`，并在 L2 之后追加 L3 段）：

```rust
        if compaction_threshold.is_some() || budget_wants_compress {
            let threshold = compaction_threshold.unwrap_or(0);
            let all_msgs = memory.get_all();
            let tokens = estimate_tokens(&all_msgs);

            if tokens > threshold || budget_wants_compress {
                let before = tokens;
                memory.shrink_large_results(threshold.max(1) as usize * 4);
                let after_shrink = estimate_tokens(&memory.get_all());

                info!("shrunk tool results: {} -> {} tokens", before, after_shrink);

                if after_shrink > threshold {
                    warn!("context still over threshold after shrinking tool results. sliding window...");
                    memory.slide_window();
                    let after_slide = estimate_tokens(&memory.get_all());
                    info!("slid window: {} -> {} tokens", after_shrink, after_slide);

                    // B2 L3：L1+L2 仍不够（或 budget 要求压缩）时，结构化摘要。
                    if l3_enabled && (after_slide > threshold || budget_wants_compress) {
                        let p: &dyn Provider = compact_provider
                            .as_deref()
                            .unwrap_or_else(|| provider.as_ref());
                        if l3.try_compact(p, memory).await {
                            let after_l3 = estimate_tokens(&memory.get_all());
                            info!("L3 compacted: {} -> {} tokens", after_slide, after_l3);
                        }
                    }
                }
            }
        }
```

并在 `for step in 0..max_steps` 之前初始化会话级压缩器：

```rust
    let mut l3 = crate::compaction::L3Compactor::new();
```

(c) **max_steps 分支**——把两处 `Err(anyhow::anyhow!("reached max steps ..."))`（`StepOutcome::MaxSteps` 臂与循环耗尽处）都替换为同一段：

```rust
                warn!("agent reached max steps ({max_steps})");
                if pause_on_max_steps {
                    tx.send(Ok(RunEvent::Paused {
                        reason: format!("reached max steps ({max_steps})"),
                        session_id: session_label.clone(),
                    }))
                    .await
                    .ok();
                    return Ok(());
                }
                return Err(anyhow::anyhow!(
                    "reached max steps ({max_steps}) without completing the task"
                ));
```

> 关键语义：pause 走 `Ok(())`，因此 `run_stream` 既有的"结束时把完整对话写回 history"路径照常执行——**暂停即已保存**，这就是断点续跑的落点（chat 模式下由 ChatPersistence 落盘；`run` 模式提示改用 `chat --resume`）。

- [ ] **Step 4: 写集成测试（agent.rs 既有 `mod tests` 的 Agent+MockProvider 区，沿用同模块既有 mock 构造）**

```rust
    #[tokio::test]
    async fn max_steps_pause_emits_paused_not_error() {
        // provider 永远只回工具调用/继续，从而耗尽 max_steps。
        // 构造方式沿用本模块上方既有 Agent+MockProvider 集成测试。
        let agent = /* 同上方测试的最小 Agent 构造 */
            .with_on_max_steps("pause")
            .with_session_label("sess-test-1");
        let mut stream = agent
            .run_stream(RunInput {
                prompt: "loop forever".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        let mut saw_paused = false;
        while let Some(ev) = stream.next().await {
            if let Ok(RunEvent::Paused { reason, session_id }) = ev {
                assert!(reason.contains("max steps"));
                assert_eq!(session_id.as_deref(), Some("sess-test-1"));
                saw_paused = true;
            }
        }
        assert!(saw_paused, "must emit Paused instead of stream error");
    }

    #[tokio::test]
    async fn max_steps_error_mode_keeps_old_behavior() {
        let agent = /* 同款最小构造 */
            .with_on_max_steps("error");
        let mut stream = agent
            .run_stream(RunInput {
                prompt: "loop forever".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        let mut saw_err = false;
        while let Some(ev) = stream.next().await {
            if ev.is_err() {
                saw_err = true;
            }
        }
        assert!(saw_err, "error mode must surface a stream error (pre-B2 contract)");
    }
```

> 上面两处 `/* 构造 */` 由实现者用**本文件既有集成测试的同款 mock provider 构造**填充（同模块内可见、多个先例）；这是对既有测试基建的引用而非留白——若既有 mock 不支持"永远继续"行为，为其加一个最小变体。

- [ ] **Step 5: 验证 + 提交**

Run: `cargo test -p deepseeknova-agent 2>&1 | grep 'test result' && cargo clippy -p deepseeknova-agent --all-targets -- -D warnings 2>&1 | tail -2`
Expected: 全 `ok`、clippy 干净

```bash
cargo fmt -p deepseeknova-agent
git add crates/deepseeknova-agent/src/agent.rs crates/deepseeknova-agent/src/lib.rs
git commit -m "feat(agent): wire budget guard + L3 compaction + on_max_steps pause into main loop"
```


---

## Task 6: runtime — build_agent 装配 B2 组件

**Files:**
- Modify: `crates/deepseeknova-runtime/src/lib.rs`（`build_agent` 内 + tests）

- [ ] **Step 1: 写失败测试** — 在 runtime 的 `mod tests` 追加：

```rust
    #[test]
    fn build_agent_applies_b2_config() {
        let mut config = Config::default();
        config.graph.enabled = false;
        config.memory.enabled = false;
        config.agent.on_max_steps = "error".into();
        config.agent.l3_compaction = false;
        config.budget.enabled = false;
        let provider = std::sync::Arc::new(stub_provider());
        // 只验证不 panic 且可构建（字段为私有，行为断言在 agent 侧已覆盖）。
        let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
        let _ = agent;
    }
```

- [ ] **Step 2: 实现** — `build_agent` 中构造 `Agent` 之后（既有 `.with_extension(...)` 装配区末尾）追加：

```rust
    // ── B2 续航：max_steps 行为 / L3 压缩 / 预算守门 ──
    agent = agent
        .with_on_max_steps(&config.agent.on_max_steps)
        .with_l3_compaction(config.agent.l3_compaction);
    if config.budget.enabled {
        agent = agent.with_budget(deepseeknova_agent::budget::controller::PromptBudgetController {
            max_total_tokens: config.budget.max_total_tokens,
            max_memory_tokens: config.budget.max_memory_tokens,
        });
    }
    // compact_model 非空时为 L3 构造专用（廉价）provider；失败仅告警，
    // L3 回退复用主 provider——压缩通路永不阻断 agent 构建。
    if !config.agent.compact_model.is_empty() {
        match deepseeknova_provider::factory::create_provider_for_model(
            config,
            &config.agent.compact_model,
        ) {
            Ok(p) => agent = agent.with_compact_provider(p.into()),
            Err(e) => tracing::warn!(
                "compact_model '{}' unavailable ({e}); L3 will use the main provider",
                config.agent.compact_model
            ),
        }
    }
```

> **待核对点**：provider 工厂的实际函数名/签名以 `crates/deepseeknova-provider/src/factory.rs` 为准（CLI 的 `resolve_provider` 走的同一工厂）；若无按模型名构造的现成函数，则改为复用 CLI 同款 `create_provider(provider_cfg_with_model_override)` 形式，语义不变（构造失败→warn+回退）。

- [ ] **Step 3: budget 模块可见性** — 若 `deepseeknova_agent::budget` 当前非 pub：`crates/deepseeknova-agent/src/lib.rs` 中 `mod budget;` 改 `pub mod budget;`（并确认 `budget/mod.rs` 内 `pub mod controller;`）。归入本 Task 提交。

- [ ] **Step 4: 验证 + 提交**

Run: `cargo test -p deepseeknova-runtime 2>&1 | grep 'test result' && cargo clippy -p deepseeknova-runtime --all-targets -- -D warnings 2>&1 | tail -2`
Expected: 全 `ok`、clippy 干净

```bash
cargo fmt -p deepseeknova-runtime -p deepseeknova-agent
git add crates/deepseeknova-runtime/src/lib.rs crates/deepseeknova-agent/src/lib.rs
git commit -m "feat(runtime): assemble B2 (on_max_steps/l3/compact_model/budget) into build_agent"
```

---

## Task 7: CLI — Paused 消费（resume 提示 + exit code 3）+ CHANGELOG

**Files:**
- Modify: `crates/deepseeknova-cli/src/main.rs`
- Modify: `crates/deepseeknova-cli/src/chat.rs`（chat 内 Paused 提示，不退出进程）
- Modify: `CHANGELOG.md`

- [ ] **Step 1: stream_events 处理 Paused** — `stream_events` 的 match 中（`Done` 臂之后、`_ => {}` 之前）加：

```rust
            deepseeknova_core::RunEvent::Paused { reason, session_id } => {
                eprintln!("\n⏸ paused: {reason}");
                match session_id {
                    Some(id) => eprintln!("resume with: deepseeknova chat --resume   (session {id})"),
                    None => eprintln!("resume with: deepseeknova chat --resume"),
                }
                // 非交互（CI/脚本）可判定的专用退出码：3 = paused。
                std::process::exit(3);
            }
```

`stream_coordinator` 同款加一臂（文案相同）。

> 设计说明：`run` 是一次性命令，pause 即进程结束，exit 3 是 spec 钉死的 CI 可判定信号；`chat` 是交互 REPL，**不**退出进程——见 Step 2。

- [ ] **Step 2: chat REPL 内 Paused 提示** — `chat.rs` 消费 RunEvent 的 match（有 `_ => {}` 兜底的那处）加：

```rust
            deepseeknova_core::RunEvent::Paused { reason, .. } => {
                println!("\n⏸ paused: {reason} — 会话已保存，直接继续输入即可接着跑");
            }
```

（chat 的 history 在 run 结束时已写回并由 ChatPersistence 落盘，用户回车继续即是天然 resume。）

- [ ] **Step 3: CHANGELOG breaking 标注** — `CHANGELOG.md` 的 Unreleased/顶部段落加：

```markdown
### ⚠ Breaking

- `agent.on_max_steps` 默认值为 `"pause"`：max_steps 耗尽不再返回错误，而是发出
  `Paused` 事件并优雅结束（CLI 非交互以退出码 3 结束并打印 resume 提示）。
  依赖旧行为的自动化请显式配置 `[agent] on_max_steps = "error"`。
```

- [ ] **Step 4: 验证 + 提交**

Run: `cargo test -p deepseeknova-cli 2>&1 | grep 'test result' && cargo clippy -p deepseeknova-cli --all-targets -- -D warnings 2>&1 | tail -2`
Expected: 全 `ok`、clippy 干净

```bash
cargo fmt -p deepseeknova-cli
git add crates/deepseeknova-cli/src/main.rs crates/deepseeknova-cli/src/chat.rs CHANGELOG.md
git commit -m "feat(cli): consume Paused (resume hint + exit code 3); changelog breaking note"
```

---

## Task 8: 集成测试 — resume 保真 + replay 校验

**Files:**
- Create: `crates/deepseeknova-store/tests/resume_fidelity.rs`

- [ ] **Step 1: 建测试文件**

```rust
//! 集成：schema v2 会话跨"重启"保真恢复，且恢复历史通过 DeepSeek-V4
//! replay 校验（B2 断点续跑的正确性根基）。

use deepseeknova_context::history::validate_replay_invariant;
use deepseeknova_core::types::{FunctionCall, ToolCall};
use deepseeknova_core::{Message, Role, RunInput};
use deepseeknova_store::SessionStore;

fn tmp_root() -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("dnv-b2-it-{}-{}", std::process::id(), nanos))
}

fn tool_turn_messages() -> Vec<Message> {
    vec![
        Message {
            role: Role::User,
            content: "read src/lib.rs".into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        },
        Message {
            role: Role::Assistant,
            content: String::new(),
            name: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_9".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: "{\"path\":\"src/lib.rs\"}".into(),
                },
            }]),
            tool_call_id: None,
            reasoning_content: Some("need the file content first".into()),
        },
        Message {
            role: Role::Tool,
            content: "pub fn x() {}".into(),
            name: None,
            tool_calls: None,
            tool_call_id: Some("call_9".into()),
            reasoning_content: None,
        },
    ]
}

#[test]
fn resumed_session_preserves_tool_fidelity_and_passes_replay_check() {
    let root = tmp_root();
    let sid = "chat-b2-fidelity";
    {
        let store = SessionStore::new(root.clone()).unwrap();
        let input = RunInput {
            prompt: "read src/lib.rs".into(),
            images: vec![],
            model_override: None,
        };
        let turn = SessionStore::build_turn(&input, 1, tool_turn_messages(), None);
        store.append(sid, &turn).unwrap();
    } // drop = 模拟进程退出
    {
        let store = SessionStore::new(root.clone()).unwrap();
        let turns = store.load(sid).unwrap();
        let restored: Vec<Message> = turns
            .iter()
            .flat_map(|t| t.messages.iter().map(Message::from))
            .collect();
        assert_eq!(restored.len(), 3);
        // 保真：assistant 的 tool_calls 与 reasoning 都在。
        let a = &restored[1];
        assert!(a.tool_calls.as_ref().is_some_and(|t| t.len() == 1));
        assert!(a.reasoning_content.is_some());
        // 正确性根基：恢复历史必须通过 replay 校验（tool 结果非孤儿、
        // load-bearing reasoning 未丢）。旧版有损恢复恰恰在这里挂。
        validate_replay_invariant(&restored)
            .expect("restored history must satisfy the V4 replay invariant");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn legacy_v1_lines_coexist_with_v2_lines_in_one_session() {
    // 同一 turns.jsonl 混合旧版（无新字段）与 v2 行，load 必须全部成功。
    let root = tmp_root();
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("chat-mixed.jsonl");
    let v1 = "{\"turn\":1,\"timestamp\":\"t\",\"input\":{\"prompt\":\"hi\",\"images\":[],\"model_override\":null},\"output\":null,\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}";
    std::fs::write(&path, format!("{v1}\n")).unwrap();
    let store = SessionStore::new(root.clone()).unwrap();
    let input = RunInput { prompt: "again".into(), images: vec![], model_override: None };
    let turn = SessionStore::build_turn(&input, 2, tool_turn_messages(), None);
    store.append("chat-mixed", &turn).unwrap();
    let turns = store.load("chat-mixed").unwrap();
    assert_eq!(turns.len(), 2);
    assert!(turns[0].messages[0].tool_calls.is_none()); // v1 行
    assert!(turns[1].messages[1].tool_calls.is_some()); // v2 行
    let _ = std::fs::remove_dir_all(&root);
}
```

> **待核对点**：`StoredTurn` 的 JSON 字段名以 store 实测为准（v1 行常量按真实 serde 输出修正——可先 `println!` 一条 build_turn 的序列化结果照抄再删）；store 的 dev-dependencies 需含 `deepseeknova-context`（若无则加 workspace 依赖）。

- [ ] **Step 2: 跑测试**

Run: `cargo test -p deepseeknova-store --test resume_fidelity 2>&1 | grep 'test result'`
Expected: `2 passed; 0 failed`

- [ ] **Step 3: 提交**

```bash
cargo fmt -p deepseeknova-store
git add crates/deepseeknova-store/tests/resume_fidelity.rs crates/deepseeknova-store/Cargo.toml
git commit -m "test(store): resume fidelity roundtrip + replay-invariant + v1/v2 coexistence"
```

---

## Task 9: 全量验收

- [ ] **Step 1: `make check`** — Expected: exit 0（fmt + clippy `-D warnings` + 全部测试 + doc）
- [ ] **Step 2: `make check-desktop`** — desktop 消费 `WireEvent`（tagged），新增 kind 不应破前端；若前端 TS 类型联合需要补 `paused` 分支（tsc 报错才补，补则连同前端测试）。Expected: exit 0
- [ ] **Step 3: 冒烟（无 API key 则跳过并记录）** — `cargo run -p deepseeknova-cli -- run --max-steps 1 "多步任务示例"`；期望：非交互下打印 `⏸ paused` + resume 提示，退出码 3（`echo $?`）
- [ ] **Step 4: 收尾提交**（若 Step 2 触发了前端补丁则单独 `fix(desktop/frontend)` 提交）

---

## 验收清单（对照 spec §B2）

| spec 要求 | 落点 |
|---|---|
| L3：L1+L2 后仍超阈值或 budget 判压缩才触发 | Task 5 (b) 触发条件 |
| 7 段结构化摘要 + 直引原文 | Task 4 `render_l3_prompt` |
| 压缩后状态重建（文件清单 + 重放末条用户消息） | Task 4 `try_compact` |
| 失败回退 L2-only、连败 3 次熔断 | Task 4 `L3Compactor`（strike 单测） |
| must_replay 保护 | Task 4 顺延分支 + Task 8 replay 校验 |
| SessionStore v2 兼容旧文件 | Task 2 serde(default) + Task 8 v1/v2 共存测试 |
| resume 保真过 replay 校验 | Task 8 集成测试 |
| budget step 边界接线（Compress/Reject） | Task 5 (a) |
| `on_max_steps="pause"` 默认 + `"error"` 逃生舱 | Task 1 + Task 5 (c) |
| CLI 非交互 exit 3 + resume 提示 | Task 7 |
| CHANGELOG breaking 标注 | Task 7 Step 3 |
| 前缀缓存不破 | 所有改动仅动 Memory/历史（volatile 区后），system prefix 零触碰 |
| 开关关闭=现状 | `l3_compaction=false`、`budget.enabled=false`、`on_max_steps="error"` 三关全闭时行为与 9cb18c1 一致 |

**范围外（有意不做）**：`run` 命令的进程内自动续跑（spec 钉死无自动续跑）；desktop 的可续跑 UI 态（桌面阶段⑤承接，本期 WireEvent 已备好）；B3 自审。

## 自审记录（fresh-eyes 后修正 3 处）

1. Task 5 压缩链把 `compaction_threshold=None && budget_wants_compress` 情况纳入（`threshold.max(1)` 防 0 乘法）；
2. Task 7 chat 与 run 的 Paused 语义分叉显式化（REPL 不退出进程）；
3. 三个「待核对点」（Provider::generate 签名 / ToolCall 字段 / StoredTurn JSON 字段名）均给出核对命令与调整规则，非留白。
