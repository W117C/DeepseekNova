# 全路径接入 ModelRouter 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** TUI/Run/Plan/Serve 五个调用点全部改经 ModelRouter 取 provider（planner→Main、executor→Task），token 全量进 CostLedger，删除旧解析链死代码。

**Architecture:** router 层加 `provider_for_maybe_model(role, override, effort)` 便捷方法消除「可选显式模型」的 7 处重复；CLI 各分支按 spec 角色映射表切换；`resolve_provider`/`resolve_provider_for_task` 变死代码后删除，`resolve_provider_cfg` 保留（baseline effort 计算用）。

**Tech Stack:** Rust workspace（deepseeknova-provider / deepseeknova-cli）。

**Spec:** `docs/superpowers/specs/2026-07-31-router-all-paths-design.md`

**基线：** main @ 8cf3ec6（已含 B2 合并；下述行号以该基线实测为准）。**注意**：B2 引入的 `config.agent.compact_model` 在 runtime 直接经 factory 建 L3 压缩 provider、绕过 router 计量——属后续统一项，本计划不触碰。

---

### Task 1: provider — `provider_for_maybe_model`

**Files:**
- Modify: `crates/deepseeknova-provider/src/router.rs`（impl ModelRouter 内、`provider_for_model` 之后；测试加在文件底部既有 tests 模块）

- [ ] **Step 1: 写失败测试**（router.rs tests 模块内追加，复用既有 `router()` 夹具）

```rust
    #[test]
    fn maybe_model_override_and_fallback() {
        let r = router();
        // None → 走角色指针（Task→small）
        r.provider_for_maybe_model(ModelRole::Task, None, None).unwrap();
        assert_eq!(r.cached_instances(), 1, "small 一个实例");
        // Some → 显式覆盖（big），仍按该角色计量
        r.provider_for_maybe_model(ModelRole::Task, Some("big"), None)
            .unwrap();
        assert_eq!(r.cached_instances(), 2, "big 新增一个实例");
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p deepseeknova-provider maybe_model`
Expected: 编译失败 `no method named 'provider_for_maybe_model'`

- [ ] **Step 3: 实现**（`provider_for_model` 方法之后）

```rust
    /// Provider for a role with an optional explicit model override:
    /// `Some(model)` routes via [`Self::provider_for_model`], `None` via
    /// [`Self::provider_for`]. Accounting stays under `role` either way.
    pub fn provider_for_maybe_model(
        &self,
        role: ModelRole,
        model_override: Option<&str>,
        effort: Option<ReasoningEffort>,
    ) -> anyhow::Result<Arc<dyn Provider>> {
        match model_override {
            Some(model) => self.provider_for_model(model, role, effort),
            None => self.provider_for(role, effort),
        }
    }
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p deepseeknova-provider` → 全部 PASS（28 个，含新测试）；`cargo clippy -p deepseeknova-provider --all-targets -- -D warnings` 零警告；`cargo fmt --all`。

- [ ] **Step 5: Commit**

```bash
git add crates/deepseeknova-provider
git commit -m "feat(provider): ModelRouter::provider_for_maybe_model 可选显式覆盖"
```

---

### Task 2: CLI — 五调用点切换 + 死代码删除

**Files:**
- Modify: `crates/deepseeknova-cli/src/main.rs`（L73/78、L141、L170、L205、L279 五处 + 两个 chat factory L241-246/相应 None 分支 + 删除 L443-475 附近的 `resolve_provider`/`resolve_provider_for_task`）

- [ ] **Step 1: Run coordinator 分支（L73-82）**

原：
```rust
                let planner_provider = resolve_provider(&config, &Some(planner_model.clone()))?;
                let executor_model = coordinator
                    .executor_model
                    .clone()
                    .or_else(|| model_args.model.clone());
                let executor_provider = resolve_provider_for_task(
                    &config,
                    &executor_model,
                    Some(deepseeknova_provider::factory::ReasoningEffort::High),
                )?;
```
改为（planner→Main、executor→Task，effort 保持现行）：
```rust
                use deepseeknova_provider::cost::ModelRole;
                let planner_provider =
                    model_router.provider_for_model(planner_model, ModelRole::Main, None)?;
                let executor_model = coordinator
                    .executor_model
                    .clone()
                    .or_else(|| model_args.model.clone());
                let executor_provider = model_router.provider_for_maybe_model(
                    ModelRole::Task,
                    executor_model.as_deref(),
                    Some(deepseeknova_provider::factory::ReasoningEffort::High),
                )?;
```
注意：该分支下方 L100-113 的 delegate 块已有 `use ... ModelRole;`（块内局部 use），外提后删除块内重复 use 以免 unused/重复告警——以 clippy 结果为准调整。

- [ ] **Step 2: Run 单代理分支（L141）**

原：
```rust
                let provider = resolve_provider(&config, &model_args.model)?;
```
改为（并把下方 `build_agent(..., None, ...)` 的第二参数换成 Task provider）：
```rust
                use deepseeknova_provider::cost::ModelRole;
                let provider = model_router.provider_for_maybe_model(
                    ModelRole::Main,
                    model_args.model.as_deref(),
                    None,
                )?;
                let task_provider = model_router.provider_for(ModelRole::Task, None)?;
```
`build_agent` 调用（原 L143-150）第二参数 `None` → `Some(task_provider)`。

- [ ] **Step 3: Plan 分支（L170）**

原：
```rust
            let provider = resolve_provider(&config, model)?;
```
改为：
```rust
            use deepseeknova_provider::cost::ModelRole;
            let provider = model_router.provider_for_maybe_model(
                ModelRole::Main,
                model.as_deref(),
                None,
            )?;
```

- [ ] **Step 4: Chat TUI 分支（L205-206）**

原：
```rust
                let provider = resolve_provider_for_task(&config, model, Some(baseline_effort))?;
                let agent = build_agent(provider, None, model.as_deref(), &config, 0, mcp_tools)?
```
改为：
```rust
                use deepseeknova_provider::cost::ModelRole;
                let provider = model_router.provider_for_maybe_model(
                    ModelRole::Main,
                    model.as_deref(),
                    Some(baseline_effort),
                )?;
                let task_provider =
                    model_router.provider_for(ModelRole::Task, Some(baseline_effort))?;
                let agent = build_agent(
                    provider,
                    Some(task_provider),
                    model.as_deref(),
                    &config,
                    0,
                    mcp_tools,
                )?
```

- [ ] **Step 5: Serve 分支（L279、L288）**

原：
```rust
            let provider = resolve_provider(&config, &None)?;
```
改为（并把 L288 `build_agent(Arc::clone(&provider), None, ...)` 第二参数换 Task）：
```rust
            use deepseeknova_provider::cost::ModelRole;
            let provider = model_router.provider_for(ModelRole::Main, None)?;
            let task_provider = model_router.provider_for(ModelRole::Task, None)?;
```
`build_agent(Arc::clone(&provider), None, ...)` → `build_agent(Arc::clone(&provider), Some(task_provider), ...)`。

- [ ] **Step 6: 两个 chat factory 消重复（L241-245 与 None 分支对应处）**

原（两处相同）：
```rust
                        let provider = match &model_name {
                            // `/model switch <name>` 显式覆盖，仍按 Main 角色计量
                            Some(m) => router.provider_for_model(m, ModelRole::Main, effort)?,
                            None => router.provider_for(ModelRole::Main, effort)?,
                        };
```
改为（两处同改）：
```rust
                        // `/model switch <name>` 显式覆盖，仍按 Main 角色计量。
                        let provider = router.provider_for_maybe_model(
                            ModelRole::Main,
                            model_name.as_deref(),
                            effort,
                        )?;
```

- [ ] **Step 7: 删除死代码**

删除 `fn resolve_provider`（约 L443-448）与 `fn resolve_provider_for_task`（约 L457-475）两个函数及其 doc 注释；保留 `resolve_provider_cfg`。删除后全文 grep 确认无残留调用：
Run: `grep -n "resolve_provider\b\|resolve_provider_for_task" crates/deepseeknova-cli/src/main.rs`
Expected: 仅剩 `resolve_provider_cfg` 定义及其调用、`resolve_provider_for_model`（config 方法，名字不同不冲突）。

- [ ] **Step 8: 验证**

Run:
```bash
cargo check -p deepseeknova-cli
cargo clippy -p deepseeknova-cli --all-targets -- -D warnings
cargo test -p deepseeknova-cli
cargo fmt --all
```
Expected: 全绿；既有 CLI 测试（含 B2 新增的 `sessions_root_honors_session_config`）无回归。若局部 `use ModelRole` 与既有块内 use 冲突，统一为各分支块首一次 use。

- [ ] **Step 9: Commit**

```bash
git add crates/deepseeknova-cli
git commit -m "feat(cli): TUI/Run/Plan/Serve 全路径接入 ModelRouter 并删除旧解析链"
```

---

### Task 3: 全量回归

- [ ] **Step 1:** Run: `make check` → Expected: fmt+clippy+test+doc 全绿（跨 crate 变更强制）。
- [ ] **Step 2:** 若有修复，单独提交 `fix(...)`。

---

## 自检

- **Spec 覆盖**：router 便捷方法（T1）、五调用点+角色映射表逐行落地（T2 S1-S5）、chat factory 消重复（T2 S6）、死代码删除与 resolve_provider_cfg 保留（T2 S7）、测试计划三项（T1 S4 / T2 S8 / T3）——spec 全节有对应任务；「不做」清单无任务引入。
- **类型一致性**：`provider_for_maybe_model(role, Option<&str>, Option<ReasoningEffort>)` 在 T1 定义、T2 六处调用签名一致；`model`/`model_args.model`/`executor_model` 均为 `Option<String>`，`.as_deref()` 取 `Option<&str>`。
- **已知不确定点**：各分支 `use` 放置以 clippy 为准微调；行号以基线 8cf3ec6 实测，执行时若再有漂移按符号定位。
