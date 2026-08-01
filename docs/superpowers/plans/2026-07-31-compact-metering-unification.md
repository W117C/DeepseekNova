# Compact 计量统一实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Agent L3 压缩的 provider 选择统一到指针体系（指针优先、compact_model 经 router 计量、无 router 场景保留直连回退），终结计量旁路与双旋钮。

**Architecture:** runtime 引入 `AgentRoleProviders { task, compact }` 与新入口 `build_agent_with_role_providers`，旧两入口委托；compact 优先级三分支（注入 > compact_model 直连回退 > 不设）；直连回退段改用 `factory::create_provider_with_model`。CLI 加 `compact_provider_for` helper（指针优先/compact_model override/Disabled effort），5 个调用点改传 roles 集合。

**Tech Stack:** Rust workspace（deepseeknova-runtime / deepseeknova-cli）。

**Spec:** `docs/superpowers/specs/2026-07-31-compact-metering-unification-design.md`

**基线：** main @ d31c3a3。行号以此实测，漂移时按符号定位。

---

### Task 1: runtime — AgentRoleProviders + 新入口 + compact 优先级

**Files:**
- Modify: `crates/deepseeknova-runtime/src/lib.rs`（新入口在 L224 `build_agent_with_task_provider` 处改造；直连段 L451-472；测试模块底部）

- [ ] **Step 1: 写失败测试**（tests 模块内追加；复用既有 `stub_provider()` 与 `CountingProvider`——后者已存在于该模块，若字段访问不便可加辅助构造）

```rust
    #[test]
    fn role_providers_compact_injection_wins_over_compact_model() {
        let mut config = Config::default();
        config.graph.enabled = false;
        config.memory.enabled = false;
        // compact_model 指向一个不存在的模型名——若直连回退被错误执行，
        // resolve 失败仅告警不报错，因此用注入路径成功构建 + 后续分支
        // 测试共同界定优先级语义。
        config.agent.compact_model = "no-such-model".into();
        let main_p = std::sync::Arc::new(stub_provider());
        let compact_p: std::sync::Arc<dyn deepseeknova_provider::Provider> =
            std::sync::Arc::new(stub_provider());
        let roles = AgentRoleProviders {
            task: None,
            compact: Some(compact_p),
        };
        let agent = build_agent_with_role_providers(
            &config,
            std::env::temp_dir(),
            main_p,
            roles,
            5,
            None,
            vec![],
        )
        .unwrap();
        let _ = agent; // 注入路径构建成功；Agent 侧字段私有，行为由 agent crate 测试覆盖
    }

    #[test]
    fn role_providers_default_falls_back_to_compact_model_path() {
        // roles 全 None + compact_model 非空 → 走 B2 直连回退（构建不 panic，
        // 解析失败仅告警）。与旧 build_agent 行为等价。
        let mut config = Config::default();
        config.graph.enabled = false;
        config.memory.enabled = false;
        config.agent.compact_model = "no-such-model".into();
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
        let _ = agent;
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p deepseeknova-runtime role_providers`
Expected: 编译失败——`AgentRoleProviders` / `build_agent_with_role_providers` 不存在。

- [ ] **Step 3: 实现**

(a) 在 `build_agent_with_task_provider` 之前新增结构体与新入口；把现函数体整体移入新入口，签名中 `task_provider` 换为 `roles: AgentRoleProviders`：

```rust
/// Role-based providers injected by callers that own a ModelRouter.
/// All fields optional; `None` falls back to legacy behaviour.
#[derive(Default)]
pub struct AgentRoleProviders {
    /// Delegate engine sub-agents (the `task` pointer).
    pub task: Option<Arc<dyn deepseeknova_provider::Provider>>,
    /// Agent L3 compaction (the `compact` pointer).
    pub compact: Option<Arc<dyn deepseeknova_provider::Provider>>,
}

/// Like [`build_agent`], but routes delegate-engine sub-agents and Agent L3
/// compaction to dedicated role providers (the `task` / `compact` model
/// pointers). Unset roles fall back to legacy behaviour.
#[allow(clippy::too_many_arguments)]
pub fn build_agent_with_role_providers(
    config: &Config,
    workspace_root: PathBuf,
    provider: Arc<dyn deepseeknova_provider::Provider>,
    roles: AgentRoleProviders,
    max_steps: usize,
    gate: Option<Arc<PermissionGate>>,
    extra_tools: Vec<Arc<dyn deepseeknova_core::Tool>>,
) -> anyhow::Result<deepseeknova_agent::Agent> {
    // …原 build_agent_with_task_provider 函数体，其中原 task_provider 一律改
    // 引用 roles.task…
}
```

(b) 原函数体内委派段的 `task_provider.as_ref()` → `roles.task.as_ref()`。

(c) 原函数体内 B2 compact 直连段（原 L451-472）改为优先级三分支：

```rust
    // Compact provider 优先级：调用方注入（经 router 计量）> agent.compact_model
    // 直连回退（无 router 的调用方，如 desktop 旧入口）> 不设（L3 复用主 provider）。
    if let Some(compact) = roles.compact {
        agent = agent.with_compact_provider(compact);
    } else if !config.agent.compact_model.is_empty() {
        // 直连回退：不经 CostLedger 计量；desktop 接入 router 后可移除。
        match config
            .resolve_provider_for_model(&config.agent.compact_model)
            .cloned()
        {
            Some(cfg) => {
                match deepseeknova_provider::factory::create_provider_with_model(
                    &cfg,
                    &config.agent.compact_model,
                    None,
                ) {
                    Ok(p) => agent = agent.with_compact_provider(p.into()),
                    Err(e) => tracing::warn!(
                        "compact_model '{}' unavailable ({e}); L3 will use the main provider",
                        config.agent.compact_model
                    ),
                }
            }
            None => tracing::warn!(
                "compact_model '{}' has no matching provider; L3 will use the main provider",
                config.agent.compact_model
            ),
        }
    }
```
（替换原手搓 `cfg.model = Some(...)` + `create_provider`；原过时注释「工厂没有按模型名构造的入口…」整段删除。）

(d) 旧入口改为委托（保持签名与 doc 不变）：

```rust
pub fn build_agent_with_task_provider(
    config: &Config,
    workspace_root: PathBuf,
    provider: Arc<dyn deepseeknova_provider::Provider>,
    task_provider: Option<Arc<dyn deepseeknova_provider::Provider>>,
    max_steps: usize,
    gate: Option<Arc<PermissionGate>>,
    extra_tools: Vec<Arc<dyn deepseeknova_core::Tool>>,
) -> anyhow::Result<deepseeknova_agent::Agent> {
    build_agent_with_role_providers(
        config,
        workspace_root,
        provider,
        AgentRoleProviders {
            task: task_provider,
            ..Default::default()
        },
        max_steps,
        gate,
        extra_tools,
    )
}
```
`build_agent`（L498-516）保持现状（它委托 with_task_provider，链路自然到位）。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p deepseeknova-runtime` → 全部 PASS（既有 16 + 新 2）；`cargo clippy -p deepseeknova-runtime --all-targets -- -D warnings` 零警告；`cargo fmt --all`。

- [ ] **Step 5: Commit**

```bash
git add crates/deepseeknova-runtime
git commit -m "feat(runtime): AgentRoleProviders 集合入口与 compact 优先级三分支"
```

---

### Task 2: CLI — compact_provider_for helper + 5 调用点接 roles

**Files:**
- Modify: `crates/deepseeknova-cli/src/main.rs`（本地 build_agent 包装 L480-501；5 个调用点 L150/L222/L271/L314/L427；helper 放 `resolve_provider_cfg` 之后；测试模块底部）

- [ ] **Step 1: 写失败测试**（main.rs 底部既有 `#[cfg(test)] mod tests` 内追加）

```rust
    #[test]
    fn compact_override_prefers_pointer_over_compact_model() {
        // 指针未设 + compact_model 非空 → override 为 compact_model
        let mut c = deepseeknova_config::Config::default();
        c.agent.compact_model = "cheap".into();
        assert_eq!(compact_override_model(&c), Some("cheap"));
        // 指针已设 → 指针胜，无 override
        c.model_pointers.compact = Some("ptr-model".into());
        assert_eq!(compact_override_model(&c), None);
        // 双无 → 无 override
        c.model_pointers.compact = None;
        c.agent.compact_model.clear();
        assert_eq!(compact_override_model(&c), None);
    }
```
（把 override 判定提为纯函数 `compact_override_model` 便于测试；`compact_provider_for` 调它。）

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p deepseeknova-cli compact_override`
Expected: 编译失败——函数不存在。

- [ ] **Step 3: 实现 helper**（`resolve_provider_cfg` 之后）

```rust
/// Compact 覆盖模型判定：指针优先；指针未设而 B2 的 agent.compact_model
/// 非空时，以该模型为显式覆盖（经 router 构建，照样计量）。
fn compact_override_model(config: &deepseeknova_config::Config) -> Option<&str> {
    if config.model_pointers.compact.is_none() && !config.agent.compact_model.is_empty() {
        Some(config.agent.compact_model.as_str())
    } else {
        None
    }
}

/// Compact 角色 provider（Agent L3 压缩用）。L3 摘要是机械任务，按 Disabled
/// 分类省 reasoning tokens（与 coordinator compact 决策一致）。
fn compact_provider_for(
    router: &deepseeknova_provider::router::ModelRouter,
    config: &deepseeknova_config::Config,
) -> anyhow::Result<Arc<dyn deepseeknova_provider::Provider>> {
    router.provider_for_maybe_model(
        deepseeknova_provider::cost::ModelRole::Compact,
        compact_override_model(config),
        Some(deepseeknova_provider::factory::ReasoningEffort::Disabled),
    )
}
```

- [ ] **Step 4: 本地 build_agent 包装升级**

签名 `task_provider: Option<...>` → `roles: deepseeknova_runtime::AgentRoleProviders`；内部改调 `build_agent_with_role_providers(config, workspace_root, provider, roles, max_steps, None, extra_tools)`；doc 注释同步（说明 task/compact 两角色路由）。

- [ ] **Step 5: 5 个调用点改传 roles**

各点把原第二实参 `Some(task_provider)` 换为：
```rust
                    deepseeknova_runtime::AgentRoleProviders {
                        task: Some(task_provider),
                        compact: Some(compact_provider_for(&model_router, &config)?),
                    },
```
逐点说明（router 绑定名与借用按现场调整）：
- L150 Run 单代理：`&model_router`
- L222 Chat TUI：`&model_router`
- L271 / L427 两个 chat factory：闭包内已有 `router`（`Arc<ModelRouter>` clone），用 `compact_provider_for(&router, cfg)?`
- L314 Serve：`&model_router`

- [ ] **Step 6: 验证**

Run:
```bash
cargo test -p deepseeknova-cli
cargo clippy -p deepseeknova-cli --all-targets -- -D warnings
cargo fmt --all
```
Expected: 全绿（既有 18 + 新 1 = 19 测试）。

- [ ] **Step 7: Commit**

```bash
git add crates/deepseeknova-cli
git commit -m "feat(cli): L3 压缩接 Compact 指针（compact_model 经 router 计量回退）"
```

---

### Task 3: 全量回归

- [ ] **Step 1:** Run: `make check` → Expected: 全绿。
- [ ] **Step 2:** 有修复则单独 `fix(...)` 提交。

---

## 自检

- **Spec 覆盖**：AgentRoleProviders/新入口/委托（T1 S3a-d）、优先级三分支（T1 S3c）、create_provider_with_model 替换与注释修正（T1 S3c）、CLI helper 与效果矩阵语义（T2 S1-S3，override 纯函数使矩阵可测）、5 调用点（T2 S5）、desktop 零影响（旧入口签名不变，T1 S3d）、测试计划三项（T1/T2/T3）——全覆盖；「不做」清单无引入。
- **类型一致性**：`AgentRoleProviders` 字段 `Option<Arc<dyn Provider>>` 与调用点构造一致；`compact_provider_for` 返回 `Result<Arc<dyn Provider>>` 装入 `Some(...)`。
- **已知不确定点**：CountingProvider 是否需要（T1 测试最终未用计数，仅构建级断言——Agent compact_provider 字段私有，行为断言由 agent crate 既有测试覆盖，spec 测试计划已预留此降级）；chat factory 闭包内 `cfg` 是 `&&Config` 还是 `&Config` 按现场调整。
