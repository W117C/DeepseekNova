# Long-Task Engine B0+B1 — orch 收编裁撤 + delegate 委派 · Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 清理双轨的 orch 实验代码（收编唯一有消费者的 `ProgressTracker` 到 core、删除 orch crate），并让模型能自主 spawn 子代理（`delegate` 工具 + 4 预设 + 并发/递归边界）。

**Architecture:** 方案 A（强化主循环，零新 crate）。B0 把 `ProgressTracker` 解耦搬到 `deepseeknova-core::progress`（脱离 SwarmConfig/Plan），desktop 改引 core，删除 orch crate 并同步权威文档。B1 新增 `DelegateEngine`（agent crate，包已接线的 `SubAgentRunner` + `Semaphore`）与 `DelegateTool`（tools crate，经 ToolContext 扩展注入，照搬 GraphHandle 先例），runtime 装配预设子代理；delegate 驱动 core `ProgressTracker` 喂 desktop 进度 UI。

**Tech Stack:** Rust、tokio（Semaphore/spawn）、async-trait、serde、clap、既有 `SubAgentRunner`/`Runner`/`ToolContext` 抽象。

---

## 范围与偏差（执行前请知悉）

**做（本计划 = B0 + B1）：** orch 收编裁撤；`delegate` 工具 + explorer/coder/tester/reviewer 预设 + Semaphore(2) 满员排队 + 禁递归 + 回传封顶；delegate 进度喂 core ProgressTracker。

**不做（后续计划）：** B2 长任务续航（L3 压缩/会话续跑/budget/max_steps pause）、B3 自审——各自单独成计划。

**相对已评审 spec 的两处偏差（读码后决定，均更 YAGNI-honest）：**
1. **`TaskComplexity` 不搬到 provider**：它零消费者，且 provider 已有 `factory::resolve_effort` 三级 reasoning_effort 解析覆盖同一理念；搬移=死代码。随 orch 一并删除，理念在 provider 既有逻辑中已实现。
2. **`ProgressTracker` 搬移时解耦**：原版依赖 `SwarmConfig`/`Plan`；搬到 core 时把 `start(&SwarmConfig)`→`start(&str, ModelRoutingInfo)`、`register_plan(&Plan)`→`register_actions(&[(String,String,String)])`，其余方法不变。desktop 继续消费 `new/report/reset`（现状：idle 快照）；用 delegate 活动实时喂进度 UI 属 desktop 阶段后续项（mutation 方法已带测试就绪）。
3. **子代理底层用 `Agent` 实例而非 `SubAgentRunner`**（用户已确认）：读码发现 `SubAgentRunner` 只转发 tool-call 事件、从不执行工具（单轮文本响应，`CoordinatorRunner` 委派同此局限）。直接复用会交付不能干活的 delegate。改为 `DelegateEngine` 构建受限工具集的 `Agent` 实例（复用已验证主循环 + graph/memory 句柄传递）；`SubAgentRunner` 保持不动（其修复超出 B1 范围）。

> 若你希望保留 `TaskComplexity`，请在执行前说明。

---

## 文件结构

| 文件 | 动作 | 职责 |
|---|---|---|
| `crates/deepseeknova-core/src/progress.rs` | 建 | 解耦版 `ProgressTracker` + 报告类型（自包含，无 orch 依赖） |
| `crates/deepseeknova-core/src/lib.rs` | 改 | `pub mod progress;` |
| `crates/deepseeknova-desktop/src/lib.rs` | 改 | `AppState.progress` 改引 `deepseeknova_core::progress::ProgressTracker` |
| `crates/deepseeknova-desktop/src/commands/subagents.rs` | 改 | `get_orch_progress` 返回 core 类型；`list_subagents` 解耦（B0 内联，B1 改列预设） |
| `crates/deepseeknova-desktop/Cargo.toml` | 改 | 删 `deepseeknova-orch` 依赖 |
| `Cargo.toml`（workspace） | 改 | members/default-members/deps 移除 orch |
| `crates/deepseeknova-orch/` | 删 | 整 crate 删除 |
| `AGENTS.md` / `DESIGN.md` / `CHANGELOG.md` | 改 | 权威清单同步、章节标注裁撤、变更记录 |
| `crates/deepseeknova-config/src/lib.rs` | 改 | `[delegate]` 配置节 + 预设 |
| `crates/deepseeknova-agent/src/delegate.rs` | 建 | `DelegateEngine`（SubAgentRunner + Semaphore + 进度） |
| `crates/deepseeknova-agent/src/lib.rs` | 改 | `pub mod delegate;` + re-export |
| `crates/deepseeknova-tools/src/delegate.rs` | 建 | `DelegateTool` + `DelegateHandle` 类型别名 |
| `crates/deepseeknova-tools/src/lib.rs` | 改 | 导出 + 注册进 `all_builtin_tools` |
| `crates/deepseeknova-runtime/src/lib.rs` | 改 | 装配 `DelegateEngine`、注入句柄、disabled 处理 |
| `crates/deepseeknova-core/tests/delegate_engine.rs` | 建 | 委派往返 + 禁递归 + 排队集成测试 |

约定：`DelegateHandle = std::sync::Arc<deepseeknova_agent::DelegateEngine>`（定义在 tools/delegate.rs，对称于 GraphHandle/MemoryHandle）。

---

## Task 1: `core::progress` — 解耦版 ProgressTracker

**Files:** Create `crates/deepseeknova-core/src/progress.rs`；Modify `crates/deepseeknova-core/src/lib.rs`

- [ ] **Step 1: 导出模块** — 在 `crates/deepseeknova-core/src/lib.rs` 的 `pub mod` 列表按字母序插入（`pub mod prefix;` 之后、`pub mod registry;` 之前）：

```rust
pub mod progress;
```

- [ ] **Step 2: 建文件（含适配后的测试）** — Create `crates/deepseeknova-core/src/progress.rs`：

```rust
//! # Progress Tracker — 多智能体执行的实时状态
//!
//! 线程安全的共享进度跟踪器，desktop 前端经 Tauri 命令轮询显示委派/子代理状态。
//! 自 `deepseeknova-orch` 收编而来，已解耦 SwarmConfig/Plan——仅依赖标准库 + serde。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;

/// 整体编排状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchStatus {
    Idle,
    Planning,
    Executing,
    Completed,
    Failed(String),
}

/// 单个动作的执行状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    Pending,
    InProgress,
    Completed,
    Failed(String),
}

/// 单个动作/任务的进度快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionProgress {
    pub action_id: String,
    pub name: String,
    pub description: String,
    pub status: ActionStatus,
    pub assigned_to: Option<String>,
    pub started_at: Option<u64>,
    pub completed_at: Option<u64>,
    pub output_summary: Option<String>,
    pub retry_count: u32,
}

/// 模型路由信息（前端展示用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRoutingInfo {
    pub planner_model: String,
    pub worker_model: String,
    pub thinking_enabled: bool,
    pub reasoning_effort: String,
}

impl Default for ModelRoutingInfo {
    fn default() -> Self {
        Self {
            planner_model: "deepseek-v4-pro".into(),
            worker_model: "deepseek-v4-flash".into(),
            thinking_enabled: true,
            reasoning_effort: "high".into(),
        }
    }
}

/// 完整编排进度报告——可序列化给前端。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchProgressReport {
    pub status: OrchStatus,
    pub goal: Option<String>,
    pub total_actions: usize,
    pub completed_actions: usize,
    pub failed_actions: usize,
    pub in_progress_actions: usize,
    pub elapsed_secs: f64,
    pub actions: Vec<ActionProgress>,
    pub model_routing: ModelRoutingInfo,
}

/// 线程安全的进度跟踪器，编排引擎与前端共享。
#[derive(Clone)]
pub struct ProgressTracker {
    inner: Arc<RwLock<TrackerState>>,
}

struct TrackerState {
    status: OrchStatus,
    goal: Option<String>,
    actions: HashMap<String, ActionProgress>,
    action_order: Vec<String>,
    start_time: Option<Instant>,
    model_routing: ModelRoutingInfo,
}

impl ProgressTracker {
    /// 新建空闲跟踪器。
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(TrackerState {
                status: OrchStatus::Idle,
                goal: None,
                actions: HashMap::new(),
                action_order: Vec::new(),
                start_time: None,
                model_routing: ModelRoutingInfo::default(),
            })),
        }
    }

    /// 开始一次编排（解耦：直接接受 goal + 路由信息，不再依赖 SwarmConfig）。
    pub fn start(&self, goal: &str, routing: ModelRoutingInfo) {
        let mut state = self.inner.write().unwrap_or_else(|e| e.into_inner());
        state.status = OrchStatus::Planning;
        state.goal = Some(goal.to_string());
        state.actions.clear();
        state.action_order.clear();
        state.start_time = Some(Instant::now());
        state.model_routing = routing;
    }

    /// 注册动作列表（解耦：(id, name, description) 元组，不再依赖 Plan）。
    pub fn register_actions(&self, actions: &[(String, String, String)]) {
        let mut state = self.inner.write().unwrap_or_else(|e| e.into_inner());
        state.status = OrchStatus::Executing;
        for (id, name, description) in actions {
            let progress = ActionProgress {
                action_id: id.clone(),
                name: name.clone(),
                description: description.clone(),
                status: ActionStatus::Pending,
                assigned_to: None,
                started_at: None,
                completed_at: None,
                output_summary: None,
                retry_count: 0,
            };
            state.action_order.push(id.clone());
            state.actions.insert(id.clone(), progress);
        }
    }

    /// 标记动作开始。
    pub fn mark_started(&self, action_id: &str, assigned_to: &str) {
        let mut state = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if let Some(action) = state.actions.get_mut(action_id) {
            action.status = ActionStatus::InProgress;
            action.assigned_to = Some(assigned_to.to_string());
            action.started_at = Some(now_epoch());
        }
    }

    /// 标记动作完成（输出截断至 200 字符摘要）。
    pub fn mark_completed(&self, action_id: &str, output: &str) {
        let mut state = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if let Some(action) = state.actions.get_mut(action_id) {
            action.status = ActionStatus::Completed;
            action.completed_at = Some(now_epoch());
            let summary = if output.chars().count() > 200 {
                let head: String = output.chars().take(200).collect();
                format!("{head}…")
            } else {
                output.to_string()
            };
            action.output_summary = Some(summary);
        }
    }

    /// 标记动作失败。
    pub fn mark_failed(&self, action_id: &str, reason: &str) {
        let mut state = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if let Some(action) = state.actions.get_mut(action_id) {
            action.status = ActionStatus::Failed(reason.to_string());
            action.completed_at = Some(now_epoch());
        }
    }

    /// 记录一次重试。
    pub fn record_retry(&self, action_id: &str) {
        let mut state = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if let Some(action) = state.actions.get_mut(action_id) {
            action.retry_count += 1;
            action.status = ActionStatus::InProgress;
        }
    }

    /// 标记整体编排结束。
    pub fn finish(&self) {
        let mut state = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let has_failures = state
            .actions
            .values()
            .any(|a| matches!(a.status, ActionStatus::Failed(_)));
        state.status = if has_failures {
            OrchStatus::Failed("some actions failed".into())
        } else {
            OrchStatus::Completed
        };
    }

    /// 重置为空闲。
    pub fn reset(&self) {
        let mut state = self.inner.write().unwrap_or_else(|e| e.into_inner());
        state.status = OrchStatus::Idle;
        state.goal = None;
        state.actions.clear();
        state.action_order.clear();
        state.start_time = None;
    }

    /// 生成前端进度报告。
    pub fn report(&self) -> OrchProgressReport {
        let state = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let elapsed = state
            .start_time
            .map(|s| s.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        let actions: Vec<ActionProgress> = state
            .action_order
            .iter()
            .filter_map(|id| state.actions.get(id).cloned())
            .collect();
        let completed = actions
            .iter()
            .filter(|a| a.status == ActionStatus::Completed)
            .count();
        let failed = actions
            .iter()
            .filter(|a| matches!(a.status, ActionStatus::Failed(_)))
            .count();
        let in_progress = actions
            .iter()
            .filter(|a| a.status == ActionStatus::InProgress)
            .count();
        OrchProgressReport {
            status: state.status.clone(),
            goal: state.goal.clone(),
            total_actions: actions.len(),
            completed_actions: completed,
            failed_actions: failed,
            in_progress_actions: in_progress,
            elapsed_secs: (elapsed * 10.0).round() / 10.0,
            actions,
            model_routing: state.model_routing.clone(),
        }
    }
}

impl Default for ProgressTracker {
    fn default() -> Self {
        Self::new()
    }
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_lifecycle() {
        let tracker = ProgressTracker::new();
        assert_eq!(tracker.report().status, OrchStatus::Idle);

        tracker.start("Build a REST API", ModelRoutingInfo::default());
        assert_eq!(tracker.report().status, OrchStatus::Planning);
        assert_eq!(tracker.report().goal.as_deref(), Some("Build a REST API"));

        tracker.register_actions(&[
            ("a1".into(), "create_schema".into(), "Create DB schema".into()),
            ("a2".into(), "write_tests".into(), "Write integration tests".into()),
        ]);
        let report = tracker.report();
        assert_eq!(report.status, OrchStatus::Executing);
        assert_eq!(report.total_actions, 2);
        assert_eq!(report.completed_actions, 0);

        tracker.mark_started("a1", "worker-1");
        assert_eq!(tracker.report().in_progress_actions, 1);
        tracker.mark_completed("a1", "Schema created successfully");
        assert_eq!(tracker.report().completed_actions, 1);

        tracker.mark_started("a2", "worker-2");
        tracker.mark_failed("a2", "test framework not found");
        assert_eq!(tracker.report().failed_actions, 1);

        tracker.finish();
        assert!(matches!(tracker.report().status, OrchStatus::Failed(_)));

        tracker.reset();
        assert_eq!(tracker.report().status, OrchStatus::Idle);
    }

    #[test]
    fn retry_tracking() {
        let tracker = ProgressTracker::new();
        tracker.start("test", ModelRoutingInfo::default());
        tracker.register_actions(&[("a1".into(), "flaky".into(), "Flaky action".into())]);
        tracker.mark_started("a1", "w1");
        tracker.record_retry("a1");
        tracker.record_retry("a1");
        assert_eq!(tracker.report().actions[0].retry_count, 2);
    }
}
```

- [ ] **Step 3: 运行测试确认通过** — Run: `cargo test -p deepseeknova-core progress:: -- --nocapture` — Expected: PASS（2 个）。

- [ ] **Step 4: clippy + fmt** — Run: `cargo clippy -p deepseeknova-core --lib -- -D warnings`（core 禁 unwrap/expect；本文件仅用 `unwrap_or_else(|e| e.into_inner())`/`unwrap_or`，合规）；`cargo fmt -p deepseeknova-core`。

- [ ] **Step 5: Commit**

```bash
git add crates/deepseeknova-core/src/progress.rs crates/deepseeknova-core/src/lib.rs
git commit -m "feat(core): salvage decoupled ProgressTracker from orch into core::progress"
```


---

## Task 2: desktop 改引 core::progress（解除 orch 引用）

**Files:** Modify `crates/deepseeknova-desktop/src/lib.rs`、`crates/deepseeknova-desktop/src/commands/subagents.rs`

> desktop 不在 `make check` 范围。验证用 `cargo check -p deepseeknova-desktop`；若因 Tauri build.rs 需要前端产物 `dist/` 而失败，先 `make frontend`。

- [ ] **Step 1: AppState 改类型** — 在 `crates/deepseeknova-desktop/src/lib.rs`，把

```rust
    /// Shared multi-agent orchestration progress tracker, polled by the UI.
    pub progress: std::sync::Arc<deepseeknova_orch::ProgressTracker>,
```

改为

```rust
    /// Shared multi-agent progress tracker, polled by the UI (fed by delegate engine).
    pub progress: std::sync::Arc<deepseeknova_core::progress::ProgressTracker>,
```

并把构造处

```rust
        progress: std::sync::Arc::new(deepseeknova_orch::ProgressTracker::new()),
```

改为

```rust
        progress: std::sync::Arc::new(deepseeknova_core::progress::ProgressTracker::new()),
```

- [ ] **Step 2: get_orch_progress 返回 core 类型** — 在 `crates/deepseeknova-desktop/src/commands/subagents.rs`，把 `get_orch_progress` 的返回类型

```rust
) -> Result<deepseeknova_orch::OrchProgressReport, String> {
```

改为

```rust
) -> Result<deepseeknova_core::progress::OrchProgressReport, String> {
```

- [ ] **Step 3: list_subagents 解耦（内联默认值，保持 JSON 输出不变）** — 把 `list_subagents` 中对 orch 的两处引用替换为内联本地值。将

```rust
    // Report the available agent roles from the orch system
    let routing = deepseeknova_orch::ModelRouting::default();
```

改为

```rust
    // Model routing labels for display (inlined; orch crate removed in B0).
    // B1 will replace the role list below with the delegate presets.
    let planner_model = "deepseek-v4-pro";
    let worker_model = "deepseek-v4-flash";
    let trivial_model = "deepseek-v4-flash";
```

把四处 `routing.planner_model` / `routing.worker_model` 分别替换为 `planner_model` / `worker_model`（保持 `queen`/`reviewer` 用 planner、`worker-code`/`researcher` 用 worker 的原映射）。将

```rust
    let swarm_config = deepseeknova_orch::SwarmConfig::default();

    Ok(serde_json::json!({
        "mock": false,
        "architecture": "Queen-led Swarm (GOAP)",
        "max_workers": swarm_config.max_workers,
        "thinking_enabled": swarm_config.thinking_enabled,
        "reasoning_effort": swarm_config.reasoning_effort,
        "model_routing": {
            "planner": routing.planner_model,
            "worker": routing.worker_model,
            "trivial": routing.trivial_model,
        },
        "agents": agents,
        "provider_count": config.providers.len(),
    }))
```

改为

```rust
    Ok(serde_json::json!({
        "mock": false,
        "architecture": "Queen-led Swarm (GOAP)",
        "max_workers": 5,
        "thinking_enabled": true,
        "reasoning_effort": "high",
        "model_routing": {
            "planner": planner_model,
            "worker": worker_model,
            "trivial": trivial_model,
        },
        "agents": agents,
        "provider_count": config.providers.len(),
    }))
```

> 这一步是纯机械解耦，输出与现状逐字节一致；B1 Task 8 会把 `agents`/`architecture` 改成 delegate 预设。

- [ ] **Step 4: 编译确认（仍依赖 orch，此步只验证 core 改引正确）** — Run: `cargo check -p deepseeknova-desktop`（必要时先 `make frontend`）。Expected: 通过（此时 orch 仍在 workspace，仅验证 desktop 不再直接引用 orch 类型；`rg "deepseeknova_orch" crates/deepseeknova-desktop/src` 应返回零）。

- [ ] **Step 5: Commit**

```bash
git add crates/deepseeknova-desktop/src/lib.rs crates/deepseeknova-desktop/src/commands/subagents.rs
git commit -m "refactor(desktop): repoint ProgressTracker to core::progress, drop orch type refs"
```

---

## Task 3: 删除 orch crate + 同步权威文档

**Files:** Modify `Cargo.toml`（workspace）、`crates/deepseeknova-desktop/Cargo.toml`、`AGENTS.md`、`DESIGN.md`、`CHANGELOG.md`；Delete `crates/deepseeknova-orch/`

- [ ] **Step 1: 删除前依赖审计（硬性）** — Run: `rg "deepseeknova[_-]orch" --type rust --type toml`。Expected：仅剩 `Cargo.toml`（workspace + desktop 清单）与 `crates/deepseeknova-orch/` 自身；`crates/*/src` 下**零** Rust 引用（Task 2 已清除 desktop 的最后引用）。若 src 下仍有引用，先解决再继续。

- [ ] **Step 2: 从 workspace 清单移除** — 在根 `Cargo.toml`：从 `[workspace] members` 删除 `"crates/deepseeknova-orch",`；从 `default-members` 删除 `"crates/deepseeknova-orch",`；从 `[workspace.dependencies]` 删除 `deepseeknova-orch = { version = "0.4.0", path = "crates/deepseeknova-orch" }`。

- [ ] **Step 3: 从 desktop 清单移除** — 在 `crates/deepseeknova-desktop/Cargo.toml` 的 `[dependencies]` 删除 `deepseeknova-orch = { workspace = true }`。

- [ ] **Step 4: 删除 crate 目录**

```bash
git rm -r crates/deepseeknova-orch
```

- [ ] **Step 5: 同步 AGENTS.md 权威清单** — 在 `AGENTS.md` §2「项目简介」的 crate 结构中删除 `deepseeknova-orch` 那一行（`# 编排层...`）。运行 `ls -d crates/*/ | wc -l` 得删除后 crate 数 N，把 §2 中"包含 N 个 crate"的计数改为该 N（保持与实际 members 一致）。

- [ ] **Step 6: 标注 DESIGN.md 裁撤** — 在 `DESIGN.md` 的「二、GOAP 规划器」与「四、Swarm 协调」两节标题下各插入一行：

```markdown
> **状态**：**已裁撤（B0）**。GOAP/Swarm 实验实现随 `deepseeknova-orch` crate 删除；本节仅作历史设计记录保留，历史实现见删除提交之前的 git 历史。多智能体能力由 `deepseeknova-agent` 的 delegate/子代理路径提供（见长任务与多智能体引擎 spec）。
```

- [ ] **Step 7: CHANGELOG 记录** — 在 `CHANGELOG.md` 顶部 Unreleased 段追加：

```markdown
### Changed
- 删除实验性 `deepseeknova-orch` crate（GOAP + Swarm，零业务调用）；其唯一有消费者的组件 `ProgressTracker` 已解耦收编至 `deepseeknova-core::progress`。多智能体能力改由 `deepseeknova-agent` 的 delegate/子代理路径提供。
```

- [ ] **Step 8: 全量验证** — 依次运行并确认全绿：

```bash
rg "deepseeknova_orch|deepseeknova-orch" --type rust --type toml   # 期望：零命中
cargo build --workspace --exclude deepseeknova-desktop
make check
make check-desktop   # 若失败因缺 dist/，先 make frontend
```

Expected：`rg` 零命中；`make check` + `make check-desktop` 全绿；`core::progress` 的迁移测试在 core 测试集中通过。

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "chore(orch): remove experimental orch crate; sync AGENTS/DESIGN/CHANGELOG"
```


---

## Task 4: `[delegate]` 配置节

**Files:** Modify `crates/deepseeknova-config/src/lib.rs`

- [ ] **Step 1: 写失败测试** — 在 `mod tests` 内追加：

```rust
    #[test]
    fn delegate_config_defaults() {
        let c = Config::default();
        assert!(c.delegate.enabled);
        assert_eq!(c.delegate.max_concurrent, 2);
        assert_eq!(c.delegate.output_cap_tokens, 2000);
        assert!(c.delegate.agents.is_empty());
    }

    #[test]
    fn delegate_config_parses_overrides() {
        let toml = "[delegate]\nenabled = false\nmax_concurrent = 3\n\n[[delegate.agents]]\nname = \"coder\"\nmax_steps = 25\n";
        let c: Config = toml::from_str(toml).unwrap();
        assert!(!c.delegate.enabled);
        assert_eq!(c.delegate.max_concurrent, 3);
        assert_eq!(c.delegate.output_cap_tokens, 2000); // 未覆盖取默认
        assert_eq!(c.delegate.agents.len(), 1);
        assert_eq!(c.delegate.agents[0].name, "coder");
        assert_eq!(c.delegate.agents[0].max_steps, Some(25));
    }
```

- [ ] **Step 2: 运行确认失败** — Run: `cargo test -p deepseeknova-config delegate_config` — Expected: 编译失败（`no field 'delegate'`）。

- [ ] **Step 3: 加字段与类型** — 在 `Config` 结构体 `pub memory: MemoryConfig,` 之后插入：

```rust
    /// 委派子代理配置（多智能体）。
    #[serde(default)]
    pub delegate: DelegateConfig,
```

在 `// Memory (closed-loop learning)` 整节之后新增：

```rust
// ---------------------------------------------------------------------------
// Delegate (multi-agent sub-agents)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegateConfig {
    /// 主开关。false = 不注册 delegate 工具，行为等同现状。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 并发子代理上限（满员时新委派排队等待）。
    #[serde(default = "default_delegate_concurrency")]
    pub max_concurrent: usize,
    /// 子代理回传摘要的 token 上限。
    #[serde(default = "default_delegate_output_cap")]
    pub output_cap_tokens: usize,
    /// 预设覆盖/新增（按 name 匹配内置预设覆盖其字段；未匹配则新增）。
    #[serde(default)]
    pub agents: Vec<DelegateAgentOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegateAgentOverride {
    pub name: String,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    #[serde(default)]
    pub max_steps: Option<usize>,
}

fn default_delegate_concurrency() -> usize {
    2
}
fn default_delegate_output_cap() -> usize {
    2000
}

impl Default for DelegateConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_concurrent: 2,
            output_cap_tokens: 2000,
            agents: Vec::new(),
        }
    }
}
```

- [ ] **Step 4: 运行确认通过** — Run: `cargo test -p deepseeknova-config delegate_config` — Expected: PASS（2 个）。

- [ ] **Step 5: Commit**

```bash
git add crates/deepseeknova-config/src/lib.rs
git commit -m "feat(config): add [delegate] section for sub-agent presets"
```

---

## Task 5: `DelegateEngine` + 预设（agent crate）

**Files:** Create `crates/deepseeknova-agent/src/delegate.rs`；Modify `crates/deepseeknova-agent/src/lib.rs`

子代理 = 受限工具集的 `Agent` 实例（见偏差 #3）。预设为静态数据（owned String，便于配置覆盖）。

- [ ] **Step 1: 导出模块** — 在 `crates/deepseeknova-agent/src/lib.rs` 的 `pub mod` 区加 `pub mod delegate;`，并在 re-export 区加 `pub use delegate::*;`。

- [ ] **Step 2: 建文件（含测试）** — Create `crates/deepseeknova-agent/src/delegate.rs`：

```rust
//! # DelegateEngine — 模型自主 spawn 子代理（Claude Code Task-tool 式）
//!
//! 子代理是受限工具集的 [`Agent`] 实例：独立上下文、真正执行工具、只回传封顶摘要、
//! 工具集不含 `delegate`（禁递归）。并发受信号量限制，满员时排队等待。

use crate::agent::Agent;
use deepseeknova_core::{RunEvent, RunInput, Runner};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio_stream::StreamExt;

/// 一个内置子代理预设。`tools` 为工具 schema 名白名单（均不含 "delegate"）。
#[derive(Debug, Clone)]
pub struct DelegatePreset {
    pub name: String,
    pub system_prompt: String,
    pub tools: Vec<String>,
    pub max_steps: usize,
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// 4 个内置预设。工具名对应真实 schema 名（read_file/bash/search_code…）；均不含 delegate。
pub fn builtin_presets() -> Vec<DelegatePreset> {
    vec![
        DelegatePreset {
            name: "explorer".into(),
            system_prompt: "You are an explorer sub-agent. Investigate and locate relevant code/facts read-only. \
                Prefer graph tools (search_code/traverse_graph/retrieve_entity) over full-file reads. \
                Return a concise findings summary.".into(),
            tools: names(&["read_file", "ls", "glob", "grep", "search_code", "traverse_graph", "retrieve_entity", "recall", "web_fetch"]),
            max_steps: 10,
        },
        DelegatePreset {
            name: "coder".into(),
            system_prompt: "You are a coder sub-agent. Implement the requested change: read, edit/write files, \
                run shell as needed. Return a concise summary of what changed.".into(),
            tools: names(&["read_file", "write_file", "edit_file", "move_file", "ls", "glob", "grep", "bash", "search_code", "traverse_graph", "retrieve_entity"]),
            max_steps: 15,
        },
        DelegatePreset {
            name: "tester".into(),
            system_prompt: "You are a tester sub-agent. Run tests / reproduce issues via shell and report results \
                concisely. Do not modify source files.".into(),
            tools: names(&["read_file", "ls", "glob", "grep", "bash"]),
            max_steps: 10,
        },
        DelegatePreset {
            name: "reviewer".into(),
            system_prompt: "You are a reviewer sub-agent. Review code read-only and report issues concisely. \
                Do not modify files.".into(),
            tools: names(&["read_file", "ls", "glob", "grep", "search_code", "traverse_graph", "retrieve_entity"]),
            max_steps: 10,
        },
    ]
}

/// 委派引擎：持有每个预设一个配置好的 [`Agent`]，并发受信号量限制。
pub struct DelegateEngine {
    agents: HashMap<String, Arc<Agent>>,
    semaphore: Arc<Semaphore>,
    output_cap_tokens: usize,
}

impl DelegateEngine {
    pub fn new(
        agents: HashMap<String, Arc<Agent>>,
        max_concurrent: usize,
        output_cap_tokens: usize,
    ) -> Self {
        Self {
            agents,
            semaphore: Arc::new(Semaphore::new(max_concurrent.max(1))),
            output_cap_tokens,
        }
    }

    /// 已注册的子代理名（供工具做友好错误提示）。
    pub fn agent_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.agents.keys().cloned().collect();
        v.sort();
        v
    }

    /// 委派一个子代理执行 goal，返回封顶后的结果摘要。
    /// 信号量满时 **排队等待**（不拒绝）。
    pub async fn run(&self, agent: &str, goal: &str) -> anyhow::Result<String> {
        let sub = self
            .agents
            .get(agent)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown sub-agent '{agent}'"))?;

        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|_| anyhow::anyhow!("delegate semaphore closed"))?;

        let input = RunInput {
            prompt: goal.to_string(),
            images: vec![],
            model_override: None,
        };
        let text = collect_final_text(sub.as_ref(), input).await?;
        Ok(cap_output(&text, self.output_cap_tokens))
    }
}

/// 驱动子 Agent 的 run_stream 并收集最终文本（与 CLI/desktop 收集方式一致）。
async fn collect_final_text(agent: &Agent, input: RunInput) -> anyhow::Result<String> {
    let mut stream = agent.run_stream(input).await?;
    let mut final_text = String::new();
    while let Some(ev) = stream.next().await {
        match ev? {
            RunEvent::TextDelta(t) => final_text.push_str(&t),
            RunEvent::Done(out) => {
                if !out.text.is_empty() {
                    final_text = out.text;
                }
            }
            _ => {}
        }
    }
    Ok(final_text)
}

/// 头尾截断到 token 预算（chars ≈ tokens×4），中部省略。
fn cap_output(text: &str, cap_tokens: usize) -> String {
    let cap_chars = cap_tokens.saturating_mul(4).max(80);
    let total = text.chars().count();
    if total <= cap_chars {
        return text.to_string();
    }
    let head: String = text.chars().take(cap_chars * 2 / 3).collect();
    let tail_n = cap_chars / 3;
    let tail: String = text
        .chars()
        .skip(total.saturating_sub(tail_n))
        .collect();
    format!("{head}\n…[delegate output truncated]…\n{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::MockProvider;

    #[test]
    fn presets_never_include_delegate_tool() {
        // 禁递归的可测形式：任何预设的工具集都不含 "delegate"。
        for p in builtin_presets() {
            assert!(
                !p.tools.iter().any(|t| t == "delegate"),
                "preset {} must not include delegate",
                p.name
            );
        }
    }

    #[test]
    fn presets_cover_four_roles() {
        let names: Vec<String> = builtin_presets().into_iter().map(|p| p.name).collect();
        for expected in ["explorer", "coder", "tester", "reviewer"] {
            assert!(names.iter().any(|n| n == expected), "missing preset {expected}");
        }
    }

    #[test]
    fn cap_output_truncates_long_and_keeps_short() {
        let long = "x".repeat(10_000);
        let out = cap_output(&long, 100);
        assert!(out.chars().count() < 10_000);
        assert!(out.contains("truncated"));
        assert_eq!(cap_output("hello", 100), "hello");
    }

    #[tokio::test]
    async fn run_delegates_to_agent_and_caps() {
        let mut agents: HashMap<String, Arc<Agent>> = HashMap::new();
        let sub = Agent::new(Arc::new(MockProvider::text("explored: found the bug in auth.rs")), 3)
            .with_system_prompt("explorer");
        agents.insert("explorer".into(), Arc::new(sub));
        let engine = DelegateEngine::new(agents, 2, 2000);

        let out = engine.run("explorer", "find the bug").await.unwrap();
        assert!(out.contains("explored"), "got: {out}");
    }

    #[tokio::test]
    async fn run_unknown_agent_errors() {
        let engine = DelegateEngine::new(HashMap::new(), 2, 2000);
        assert!(engine.run("nope", "x").await.is_err());
    }
}
```

- [ ] **Step 3: 运行测试确认通过** — Run: `cargo test -p deepseeknova-agent delegate:: -- --nocapture` — Expected: PASS（5 个）。

- [ ] **Step 4: clippy + fmt** — `cargo clippy -p deepseeknova-agent --all-targets -- -D warnings`；`cargo fmt -p deepseeknova-agent`。

- [ ] **Step 5: Commit**

```bash
git add crates/deepseeknova-agent/src/delegate.rs crates/deepseeknova-agent/src/lib.rs
git commit -m "feat(agent): DelegateEngine + 4 sub-agent presets (Agent-backed, no recursion)"
```


---

## Task 6: `DelegateTool`（tools crate）

**Files:** Create `crates/deepseeknova-tools/src/delegate.rs`；Modify `crates/deepseeknova-tools/src/lib.rs`、`crates/deepseeknova-tools/Cargo.toml`

- [ ] **Step 1: 加依赖** — 在 `crates/deepseeknova-tools/Cargo.toml` 的 `[dependencies]` 加（tools→agent 单向，无环：agent 不依赖 tools）：

```toml
deepseeknova-agent = { workspace = true }
```

- [ ] **Step 2: 建文件** — Create `crates/deepseeknova-tools/src/delegate.rs`：

```rust
//! delegate 工具：把子任务委派给独立子代理（explorer/coder/tester/reviewer）。
//! 引擎句柄经 `ToolContext.extensions` 注入（`DelegateHandle`），缺失时优雅降级。

use async_trait::async_trait;
use deepseeknova_agent::DelegateEngine;
use deepseeknova_core::{Tool, ToolContext, ToolSchema};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

/// 共享委派引擎句柄（runtime 注入，对称于 Graph/MemoryHandle）。
pub type DelegateHandle = Arc<DelegateEngine>;

const NO_DELEGATE_MSG: &str = "委派引擎未启用（[delegate] enabled=false 或未装配）。";

fn handle(ctx: &ToolContext) -> Option<DelegateHandle> {
    ctx.extensions.get::<DelegateHandle>().cloned()
}

pub struct DelegateTool;

#[derive(Deserialize)]
struct DelegateArgs {
    agent: String,
    goal: String,
    #[serde(default)]
    context: Option<String>,
}

#[async_trait]
impl Tool for DelegateTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "delegate".to_string(),
            description: "把一个自包含子任务委派给独立子代理执行，返回其结果摘要。子代理有独立上下文、\
                不能再委派（禁递归）。agent 取值：explorer（只读调研）、coder（改代码）、tester（跑测试）、reviewer（只读审查）。".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "agent": {
                        "type": "string",
                        "enum": ["explorer", "coder", "tester", "reviewer"],
                        "description": "Preset sub-agent to delegate to."
                    },
                    "goal": {"type": "string", "description": "Self-contained task for the sub-agent."},
                    "context": {"type": "string", "description": "Optional extra context prepended to the goal."}
                },
                "required": ["agent", "goal"]
            }),
        }
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        if ctx.cancellation.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let parsed: DelegateArgs = serde_json::from_str(args)?;
        let h = match handle(ctx) {
            Some(h) => h,
            None => return Ok(NO_DELEGATE_MSG.to_string()),
        };
        let goal = match parsed.context {
            Some(c) if !c.is_empty() => format!("{c}\n\n{}", parsed.goal),
            _ => parsed.goal.clone(),
        };
        match h.run(&parsed.agent, &goal).await {
            Ok(text) => Ok(format!("[delegate:{}] {text}", parsed.agent)),
            Err(e) => Ok(format!(
                "delegate to '{}' failed: {e}. Available agents: {}",
                parsed.agent,
                h.agent_names().join(", ")
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_agent::{Agent, DelegateEngine};
    use std::collections::HashMap;

    fn ctx_with_engine() -> ToolContext {
        let mut agents: HashMap<String, Arc<Agent>> = HashMap::new();
        agents.insert(
            "explorer".into(),
            Arc::new(
                Agent::new(Arc::new(deepseeknova_agent::test_utils::MockProvider::text(
                    "found the config in lib.rs",
                )), 3)
                .with_system_prompt("explorer"),
            ),
        );
        let engine: DelegateHandle = Arc::new(DelegateEngine::new(agents, 2, 2000));
        ToolContext::new("t").with_extension(engine)
    }

    #[tokio::test]
    async fn delegate_runs_named_agent() {
        let ctx = ctx_with_engine();
        let out = DelegateTool
            .execute(&ctx, r#"{"agent":"explorer","goal":"find config"}"#)
            .await
            .unwrap();
        assert!(out.contains("[delegate:explorer]"), "got: {out}");
        assert!(out.contains("found the config"));
    }

    #[tokio::test]
    async fn delegate_unknown_agent_is_friendly() {
        let ctx = ctx_with_engine();
        let out = DelegateTool
            .execute(&ctx, r#"{"agent":"nope","goal":"x"}"#)
            .await
            .unwrap();
        assert!(out.contains("failed"), "got: {out}");
        assert!(out.contains("Available agents: explorer"));
    }

    #[tokio::test]
    async fn delegate_degrades_without_handle() {
        let ctx = ToolContext::new("t");
        let out = DelegateTool
            .execute(&ctx, r#"{"agent":"explorer","goal":"x"}"#)
            .await
            .unwrap();
        assert!(out.contains("未启用"), "got: {out}");
    }
}
```

- [ ] **Step 3: 导出 + 注册** — 在 `crates/deepseeknova-tools/src/lib.rs`：加 `pub mod delegate;` 与 `pub use delegate::*;`；在 `all_builtin_tools_with_sandbox` 的 vec 末尾加 `Arc::new(DelegateTool),`。

- [ ] **Step 4: 运行测试 + clippy** — `cargo test -p deepseeknova-tools delegate:: -- --nocapture`（3 个）；`cargo clippy -p deepseeknova-tools --all-targets -- -D warnings`；`cargo fmt -p deepseeknova-tools`。

- [ ] **Step 5: Commit**

```bash
git add crates/deepseeknova-tools/src/delegate.rs crates/deepseeknova-tools/src/lib.rs crates/deepseeknova-tools/Cargo.toml
git commit -m "feat(tools): delegate tool backed by DelegateEngine handle"
```

---

## Task 7: runtime 装配 delegate（句柄提升 + 子代理构建）

**Files:** Modify `crates/deepseeknova-runtime/src/lib.rs`

- [ ] **Step 1: 主 agent 构建改用克隆（保留 provider/security/gate 供子代理复用）** — 在 `build_agent` 中：

将 `let mut agent = deepseeknova_agent::Agent::new(provider, steps)` 改为 `Arc::clone(&provider)`：

```rust
    let mut agent = deepseeknova_agent::Agent::new(Arc::clone(&provider), steps)
        .with_workspace_root(workspace_root.clone())
        .with_security(security.clone());
```

将权限门控块

```rust
    let gate = gate.or_else(|| permission_gate_for(config, &workspace_root));
    if let Some(gate) = gate {
        agent = agent.with_permission_gate(gate);
    }
```

改为（用 `ref` 借用，保留 `gate` Option 供子代理）：

```rust
    let gate = gate.or_else(|| permission_gate_for(config, &workspace_root));
    if let Some(ref g) = gate {
        agent = agent.with_permission_gate(g.clone());
    }
```

- [ ] **Step 2: delegate 工具禁用开关** — 在记忆工具禁用块（`if !config.memory.enabled { ... }`）之后追加：

```rust
    // 委派关闭时排除 delegate 工具。
    if !config.delegate.enabled {
        disabled.insert("delegate");
    }
```

- [ ] **Step 3: 句柄提升** — 在 graph 装配块（`if config.graph.enabled {`）之前插入：

```rust
    // 句柄提升到外层，供主 agent 与子代理共享（delegate 需要）。
    let mut graph_ext: Option<deepseeknova_tools::GraphHandle> = None;
    let mut memory_ext: Option<deepseeknova_tools::MemoryHandle> = None;
```

在 graph 块内 `agent = agent.with_extension(handle.clone());` 之后追加 `graph_ext = Some(handle.clone());`。
在 memory 块内 `agent = agent.with_extension(handle.clone());` 之后追加 `memory_ext = Some(handle.clone());`。

- [ ] **Step 4: delegate 装配块** — 在 `Ok(agent)` 之前插入：

```rust
    // ── 委派引擎：为每个预设构建受限工具集的子 Agent（共享 graph/memory 句柄）──
    if config.delegate.enabled {
        let engine = build_delegate_engine(
            config,
            Arc::clone(&provider),
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

- [ ] **Step 5: 新增 helper 函数** — 在 `build_agent` 之后（`#[cfg(test)] mod tests` 之前）新增：

```rust
/// 构建委派引擎：合并内置预设与配置覆盖，为每个预设造一个受限工具集的子 Agent
/// （共享主 agent 的 graph/memory 句柄与安全策略）。禁递归：剔除任何 "delegate" 工具。
#[allow(clippy::too_many_arguments)]
fn build_delegate_engine(
    config: &Config,
    provider: Arc<dyn deepseeknova_provider::Provider>,
    workspace_root: &std::path::Path,
    security: &SecurityContext,
    gate: Option<Arc<PermissionGate>>,
    graph_ext: Option<deepseeknova_tools::GraphHandle>,
    memory_ext: Option<deepseeknova_tools::MemoryHandle>,
) -> Arc<deepseeknova_agent::DelegateEngine> {
    use deepseeknova_core::Tool;

    // 子代理工具源（沿用主 agent 的沙箱选择）。
    let base: Vec<Arc<dyn Tool>> = if config.sandbox.enabled {
        let sandbox: Arc<dyn deepseeknova_sandbox::Sandbox> =
            Arc::from(deepseeknova_sandbox::platform_sandbox_with(
                &config.sandbox.writable_paths,
                config.sandbox.allow_network,
            ));
        deepseeknova_tools::all_builtin_tools_with_sandbox(sandbox)
    } else {
        deepseeknova_tools::all_builtin_tools()
    };

    // 合并内置预设 + 配置覆盖（按 name 匹配覆盖字段，未匹配则新增）。
    let mut presets = deepseeknova_agent::builtin_presets();
    for ov in &config.delegate.agents {
        if let Some(p) = presets.iter_mut().find(|p| p.name == ov.name) {
            if let Some(sp) = &ov.system_prompt {
                p.system_prompt = sp.clone();
            }
            if let Some(tools) = &ov.tools {
                p.tools = tools.clone();
            }
            if let Some(ms) = ov.max_steps {
                p.max_steps = ms;
            }
        } else {
            presets.push(deepseeknova_agent::DelegatePreset {
                name: ov.name.clone(),
                system_prompt: ov.system_prompt.clone().unwrap_or_default(),
                tools: ov.tools.clone().unwrap_or_default(),
                max_steps: ov.max_steps.unwrap_or(10),
            });
        }
    }

    let mut agents: std::collections::HashMap<String, Arc<deepseeknova_agent::Agent>> =
        std::collections::HashMap::new();
    for p in &presets {
        // 禁递归：即便配置误加 "delegate" 也剔除。
        let sub_tools: Vec<Arc<dyn Tool>> = base
            .iter()
            .filter(|t| {
                let n = t.schema().name;
                n != "delegate" && p.tools.iter().any(|allow| allow == &n)
            })
            .cloned()
            .collect();
        let mut sub = deepseeknova_agent::Agent::new(Arc::clone(&provider), p.max_steps)
            .with_workspace_root(workspace_root.to_path_buf())
            .with_security(security.clone())
            .with_system_prompt(p.system_prompt.clone());
        for t in sub_tools {
            sub.register_tool(t);
        }
        if let Some(g) = &graph_ext {
            sub = sub.with_extension(g.clone());
        }
        if let Some(m) = &memory_ext {
            sub = sub.with_extension(m.clone());
        }
        if let Some(gate) = &gate {
            sub = sub.with_permission_gate(gate.clone());
        }
        agents.insert(p.name.clone(), Arc::new(sub));
    }

    Arc::new(deepseeknova_agent::DelegateEngine::new(
        agents,
        config.delegate.max_concurrent,
        config.delegate.output_cap_tokens,
    ))
}
```

- [ ] **Step 6: 写测试** — 在 runtime `mod tests` 追加：

```rust
    #[tokio::test]
    async fn build_agent_registers_delegate_tool_when_enabled() {
        let mut config = Config::default();
        config.delegate.enabled = true;
        config.graph.enabled = false;
        config.memory.enabled = false;
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None).unwrap();
        assert!(agent.tool_names().iter().any(|n| n == "delegate"));
    }

    #[test]
    fn build_agent_skips_delegate_when_disabled() {
        let mut config = Config::default();
        config.delegate.enabled = false;
        config.graph.enabled = false;
        config.memory.enabled = false;
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None).unwrap();
        assert!(!agent.tool_names().iter().any(|n| n == "delegate"));
    }
```

- [ ] **Step 7: 验证 + 提交** — `cargo test -p deepseeknova-runtime build_agent_registers_delegate build_agent_skips_delegate`（2 个）；`cargo clippy -p deepseeknova-runtime --all-targets -- -D warnings`；`cargo fmt -p deepseeknova-runtime`。

```bash
git add crates/deepseeknova-runtime/src/lib.rs
git commit -m "feat(runtime): wire DelegateEngine (shared handles, sub-agent presets)"
```

---

## Task 8: 排队并发集成测试

**Files:** Create `crates/deepseeknova-agent/tests/delegate_queue.rs`

- [ ] **Step 1: 写集成测试** — Create `crates/deepseeknova-agent/tests/delegate_queue.rs`：

```rust
//! 集成：并发委派在信号量满时排队（不失败、不死锁）。

use deepseeknova_agent::test_utils::MockProvider;
use deepseeknova_agent::{Agent, DelegateEngine};
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_delegates_queue_and_complete() {
    let mut agents: HashMap<String, Arc<Agent>> = HashMap::new();
    agents.insert(
        "explorer".into(),
        Arc::new(Agent::new(Arc::new(MockProvider::text("done-a")), 3).with_system_prompt("x")),
    );
    agents.insert(
        "coder".into(),
        Arc::new(Agent::new(Arc::new(MockProvider::text("done-b")), 3).with_system_prompt("x")),
    );
    // 并发上限 1 → 第二个委派必须排队等待，二者都应成功完成。
    let engine = Arc::new(DelegateEngine::new(agents, 1, 2000));
    let (e1, e2) = (engine.clone(), engine.clone());
    let (r1, r2) = tokio::join!(
        tokio::spawn(async move { e1.run("explorer", "g1").await }),
        tokio::spawn(async move { e2.run("coder", "g2").await }),
    );
    assert!(r1.unwrap().is_ok(), "first delegate should complete");
    assert!(r2.unwrap().is_ok(), "queued delegate should complete");
}
```

- [ ] **Step 2: 运行确认通过** — Run: `cargo test -p deepseeknova-agent --test delegate_queue -- --nocapture` — Expected: PASS（1 个）。

- [ ] **Step 3: Commit**

```bash
git add crates/deepseeknova-agent/tests/delegate_queue.rs
git commit -m "test(agent): concurrent delegate queueing integration"
```

---

## Task 9: 全量验收

- [ ] **Step 1: 全库检查** — Run: `make check`。Expected: fmt + clippy(`-D warnings`) + test + doc 全绿（不含 desktop）。
- [ ] **Step 2: 桌面端验证** — Run: `make check-desktop`（必要时先 `make frontend`）。Expected: 全绿（B0 的 desktop 改引 + orch 删除后 desktop 仍编译/测试通过）。
- [ ] **Step 3: 端到端冒烟（真实 provider，需 API key）** — `cargo run -p deepseeknova-cli -- run "先 delegate 一个 explorer 调研 src/ 结构，再总结三点"`。Expected: 主输出出现 `[delegate:explorer] …` 封顶摘要；子代理确实调用了检索/读文件工具（日志可见）。
- [ ] **Step 4: 最终提交（若有格式化改动）**

```bash
git add -A
git commit -m "chore(long-task-engine): B0+B1 complete"
```

---

## 偏差与取舍（汇总，执行前知悉）

1. `TaskComplexity` 不搬 provider（零消费者 + provider 已有 `resolve_effort`），随 orch 删除。
2. `ProgressTracker` 解耦搬 core；desktop 消费 `report()`；delegate 实时喂进度属 desktop 阶段后续项。
3. 子代理底层用 `Agent` 实例而非 `SubAgentRunner`（后者不执行工具）。
4. B1 不改 desktop `list_subagents` 的角色文案（B0 已解耦为可编译内联）；把它改成列 delegate 预设属 desktop 阶段。

## Spec 覆盖矩阵

| Spec 节 | 本计划落点 | 状态 |
|---|---|---|
| §4 B0 收编 ProgressTracker | Task 1 | ✓ |
| §4 B0 收编 TaskComplexity | — | 偏差 1：随 orch 删除 |
| §4 B0 依赖审计 + 删 crate + 文档同步 | Task 2/3 | ✓ |
| §5 B1 delegate 工具 + 4 预设 | Task 4/5/6 | ✓ |
| §5 Semaphore(2) 满员排队 | Task 5（run/semaphore）+ Task 8 | ✓ |
| §5 禁递归 | Task 5（预设不含 delegate）+ Task 7（filter 剔除） | ✓ |
| §5 回传封顶 | Task 5（cap_output） | ✓ |
| §5 子代理不挂沉淀/召回 | Task 7（子 Agent 不设 distill/recall） | ✓ |
| §5 runtime 装配 + disabled | Task 7 | ✓ |
| §（子代理真正执行工具 + 句柄传递） | Task 7（Agent 实例 + 句柄提升） | ✓（偏差 3） |
| B2/B3 | — | 后续计划 |

## 自审确认（writing-plans 自检）

- **Spec 覆盖**：B0/B1 每条需求有对应 Task；两处偏差（TaskComplexity、SubAgentRunner）显式记录。
- **占位符扫描**：无 TODO/TBD；每步含完整代码/命令/预期。
- **类型一致性**：`ProgressTracker`（start/register_actions 新签名）、`DelegateEngine::new(agents,max_concurrent,cap)`、`DelegatePreset{name,system_prompt,tools,max_steps}`、`DelegateHandle=Arc<DelegateEngine>`、`build_delegate_engine(...)` 在定义处与 runtime/tools 调用处逐一对齐；工具名（read_file/bash/search_code…）经读码核实。

---

## 执行方式

**Plan complete and saved to `docs/superpowers/plans/2026-07-28-long-task-engine-b0b1.md`. 两种执行方式：**

**1. 子代理驱动（推荐）** —— 每个 Task 派新子代理实现，Task 间两阶段审查。

**2. 内联执行** —— 本会话按 executing-plans 分批 + 检查点。

**选哪种？**
