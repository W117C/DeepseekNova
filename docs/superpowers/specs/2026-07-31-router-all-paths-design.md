# 全路径接入 ModelRouter 设计（TUI/Run/Plan/Serve）

- 日期：2026-07-31
- 状态：已确认（用户批准）
- 前置：一期「多模型指针与成本分账」（spec 2026-07-29-model-pointers-cost-ledger-design.md）
  与 Compact 指针接线补丁均已合入 main

## 背景

一期只把 chat 文本 REPL 接入了 ModelRouter，main.rs 中仍有 5 个调用点走旧的
`resolve_provider` / `resolve_provider_for_task` 解析链，导致这些路径的模型选择
不受 `[model_pointers]` 指针约束、token 用量不进 CostLedger：

| 调用点 | 路径 |
| --- | --- |
| L73 / L78 | Run coordinator（planner / executor） |
| L141 | Run 单代理 |
| L170 | Plan |
| L205 | Chat TUI |
| L279 | Serve |

「可选显式模型覆盖」的 match 模式已在 chat factory 重复两次，5 个新调用点会再重复。

## 方案选型

| 方案 | 结论 |
| --- | --- |
| A：router 增便捷方法 + CLI 全切换（**选定**） | 一次消除 7 处重复；desktop 未来可复用 |
| B：纯 CLI 本地 helper | 重复语义留在 CLI，desktop 之后要重写 |
| C：只包 MeteredProvider 不统一路由 | 无指针语义，弃 |

## Router 层（deepseeknova-provider/src/router.rs）

新增便捷方法（带 rustdoc 与单测）：

```rust
/// Provider for a role with an optional explicit model override:
/// `Some(model)` routes via [`provider_for_model`], `None` via
/// [`provider_for`]. Accounting stays under `role` either way.
pub fn provider_for_maybe_model(
    &self,
    role: ModelRole,
    model_override: Option<&str>,
    effort: Option<ReasoningEffort>,
) -> anyhow::Result<Arc<dyn Provider>>
```

## CLI 调用点切换（角色映射）

coordinator 角色映射（已确认）：**planner→Main，executor→Task**，不新增角色。

| 路径 | Main 来源 | Task 来源 | effort |
| --- | --- | --- | --- |
| Run coordinator | planner：`provider_for_model(planner_model, Main, None)` | executor：`provider_for_maybe_model(executor_model.or(model), Task, High)` | 保持现行 |
| Run 单代理 | `provider_for_maybe_model(model, Main, None)` | `provider_for(Task, None)` 传入 build_agent（现为 None） | 默认 |
| Plan | `provider_for_maybe_model(model, Main, None)` | 无（PlanModeRunner 单 provider） | 默认 |
| Chat TUI | `provider_for_maybe_model(model, Main, Some(baseline))` | `provider_for(Task, Some(baseline))` | 现行 baseline |
| Serve | `provider_for(Main, None)` | `provider_for(Task, None)` | 默认 |

chat 文本 REPL 的两个 agent_factory 同步改用 `provider_for_maybe_model`
（消重复，行为不变）。

## 清理

- `resolve_provider` / `resolve_provider_for_task` 在切换后无调用者，删除
- `resolve_provider_cfg`（baseline effort 计算所需）保留

## 行为边界

- 显式模型未定义于 `[[models]]` 时，`resolve_provider_for_model` 回落首个
  provider——与旧链路行为一致，零行为回归
- 所有路径的 token 从此按 Main/Task 角色计入共享 CostLedger；Serve/TUI 暂无
  查看入口，`/cost` 仅文本 REPL 可见（本期决策：只接线不加报表端点/视图）

## 明确不做（YAGNI）

- Serve 的 GET /v1/cost 成本端点（已确认不加）
- TUI 成本视图
- Planner/Executor 新计量角色（已确认复用 Main/Task）
- desktop 端接入（后续单独任务）

## 测试计划

- router：`provider_for_maybe_model` 单测（override / 非 override 两分支，
  复用既有 router 测试夹具）
- CLI：`cargo check` + `cargo clippy --all-targets -- -D warnings` + 既有
  17 测试无回归
- 回归：`make check` 全量（跨 crate 变更强制项）

## 假设与置信度

- 置信度：**高**
- 已验证：5 个调用点行号与现状（合并后 main.rs 实测）；`provider_for_model` /
  `provider_for` 语义与缓存键隔离已有测试覆盖；`resolve_provider_cfg` 与
  baseline effort 的耦合关系
- 残余风险（低）：Run coordinator 分支 planner 构建原带 `None` effort、
  executor 带 `High`，切换后保持同值，行为面仅增加计量包装
