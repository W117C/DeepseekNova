# 实现计划：任务质量闭环（治理钩子 + 策略评估 + 诊断 + 评分卡）

> 依据：`docs/superpowers/specs/2026-08-05-task-quality-loop-design.md`（已批准）
> 执行模式：worker A（阶段 1+2 治理层）先行 → 完成后 worker B（阶段 3 诊断）+ worker C（阶段 4 评分卡）并行 → 父级收尾 + `make check` 强制验收
> 约定：worker 只编辑自己分配的文件；工作区存在他人未提交改动（08-04 迭代实现），不得回退，须在其基础上工作
> 状态（2026-08-05 补记）：计划已执行完毕，实现合入 main（ca814c1 / 2bb9909）；
> 审查修复轮 5d009f4

---

## Summary

把 permission gate 升级为可编程 ToolHook 链（before 拦截/建议 + after 确定性评估，fail-open），新增写后策略评估（0 token 确定性规则短路 LLM review）、失败结构化诊断报告（serve 端点）与四维评分卡（跨会话聚合），四块共用 RunEvent 流串成任务质量闭环。

## 设计修正（实现前必须落实，同步更新设计文档 §3.2）

**`ToolHook::before` 返回值修正**：设计文档写 `-> Decision`（permission::Decision），但 `permission/Cargo.toml` 依赖 core（L21），core 不能反向依赖 permission。修正为 **core 自有枚举 `HookVerdict`**：

```rust
// core/src/tool_hook.rs
pub enum HookVerdict {
    Allow,
    AllowWith(String), // 放行 + 附加提示（进事件流）
    Ask(String),       // 升级人工审批（复用 approval 桥）
    Deny(String),      // 拒绝
}
```

agent 层把 `HookVerdict` 映射到 `permission::Decision`（决策合并：任一 Deny → 拒绝；无 Deny 且任一 Ask → Ask；全 Allow → Allow）。

## 变更分组

### A. 治理层：ToolHook 链 + 写后策略评估（worker A，先行）
**文件**：`crates/deepseeknova-core/src/tool_hook.rs`（新）、`crates/deepseeknova-core/src/runner.rs`、`crates/deepseeknova-core/src/lib.rs`（导出）、`crates/deepseeknova-security/src/quality.rs`（新）、`crates/deepseeknova-security/src/lib.rs`（导出）、`crates/deepseeknova-agent/src/quality.rs`（新）、`crates/deepseeknova-agent/src/agent.rs`、`crates/deepseeknova-runtime/src/lib.rs`、`crates/deepseeknova-config/src/lib.rs`（`[quality]` 配置字段，A 的 attach_quality_hook 依赖）

- `tool_hook.rs`：`ToolHook` trait（`name` / `interested`（窄范围 bail，默认 true）/ `before` / `after`）+ `NoopToolHook` + `HookVerdict` + `FindingSeverity{Info,Warning,Blocking}` + `QualityFinding{rule,severity,passed,evidence}`（**类型归属 core**，见修正项；全部 `///` 文档注释）
- `runner.rs`：`RunEvent` 新增 `QualityFinding(QualityFinding)` 变体（`WireEvent` 同步 tag=kind）
- `security/src/quality.rs`：`QualityRule{id,severity,kind:Regex|PathGlob|SizeLimit,message}` + `QualityPolicy::builtin()`（内置规则：no-commit-secret 正则、禁行路径 glob、单文件大小上限）+ `evaluate(&str diff, &[PathBuf]) -> Vec<QualityFinding>`（产出 core::QualityFinding）
- `agent/src/quality.rs`：`QualityHook` 实现 `ToolHook`——before 阶段对写类工具目标路径跑 PathGlob 规则（命中禁行区 → `HookVerdict::Deny`）；after 阶段对写类工具结果（diff 文本或文件内容）跑 `QualityPolicy::evaluate`，产出 findings
- `agent.rs`：
  - `Agent` 增 `tool_hooks: Vec<Arc<dyn ToolHook>>` + `with_tool_hook(Arc<dyn ToolHook>)` builder（跟随 `with_lesson_hook` 先例 agent.rs:457-466）
  - 主循环工具调用处（agent.rs:1635 gate 预检之后、1959 执行之前）：依次调用 hooks `before`，决策与 gate 决策合并；执行后调用 `after`，findings 经 `tx.send(RunEvent::QualityFinding)` 进事件流
  - review 短路（agent.rs:1267 `run_review_pass` 调用处）：本会话存在 Blocking 级 finding 时才进入 review；仅 Warning/无 finding 直接跳过 review（短路条件）
- `runtime/lib.rs`：`attach_quality_hook(agent, config, gate) -> Agent`（attach_metrics_hook 范式 L1150-1172）：`[quality] enabled` 配置开关（默认开）、组装 QualityHook 注入；quality rules 配置暂用内置集（config 键 `[quality]` 仅 enabled，规则定制列后续迭代）
- `config/src/lib.rs`：`QualityConfig { enabled: bool }`（`[quality]` 节，默认 enabled=true），serde Deserialize + Default

**A 的依赖**：security 不依赖 agent；agent 依赖 security（已依赖，agent/Cargo.toml）；core 零新依赖。

### B. 诊断层：结构化失败报告（worker B，A 完成后并行）
**文件**：`crates/deepseeknova-agent/src/diagnose.rs`（新）、`crates/deepseeknova-agent/src/agent.rs`（失败路径收集点）、`crates/deepseeknova-runtime/src/lib.rs`（装配）、`crates/deepseeknova-serve/src/lib.rs`（端点）

- `diagnose.rs`：`DiagnoseReport{session_id, outcome, phases: Vec<PhaseSpan>, failures: Vec<FailureDetail>, sub_agents: Vec<SubAgentSpan>, quality: Vec<QualityFinding>}` + serde Serialize/Deserialize
- `agent.rs`：主循环失败/Paused 路径（reflect_retry 前 agent.rs:834 附近）收集摘要：阶段时间戳（plan/tool/verify/reflect 起点终点）、失败详情（工具名/错误摘要/归因 root_cause+fix_plan 若已产出）、子代理链（preset/outcome/duration）、本会话 findings（复用 A 的事件收集）→ 生成 `DiagnoseReport` → 经诊断回调传出
- `runtime/lib.rs`：`attach_diagnose_hook(agent, config, dir) -> Agent`（回调闭包写 `<dir>/diagnose/<session_id>.json`，复用 `enforce_metrics_retention` 留存，`[metrics] dir` 取目录）；仅在 outcome != success 时落盘
- `serve/lib.rs`：`Server::with_metrics_dir(mut self, dir: PathBuf)`（builder，默认 None）+ `GET /v1/sessions/{id}/diagnose`（读文件，存在 → 200 JSON，缺失/未配置 → 404）；serve Cargo.toml 加 `deepseeknova-metrics` 依赖（叶子 crate，无循环）——仅用类型反序列化

### C. 评分层：四维评分卡（worker C，A 完成后并行）
**文件**：`crates/deepseeknova-metrics/src/lib.rs`（Scorecard + 查询）、`crates/deepseeknova-agent/src/agent.rs`（hook 签名扩展）、`crates/deepseeknova-runtime/src/lib.rs`（组装落盘）、`crates/deepseeknova-serve/src/lib.rs`（端点）

- `metrics/src/lib.rs`：`Scorecard{session_id, started_at_ms, dimensions: ScoreDimensions}`、`ScoreDimensions{governance, verification, reflection, review}`（公式见设计 §5.2）、`Scorecard::overall()`、`list_scorecards(dir) -> Vec<Scorecard>`、`aggregate(dir) -> ScorecardAggregate`（均值/趋势/最差维度）、`write_scorecard(report, dir)` 写 `<dir>/<session_id>.scorecard.json`（**独立文件，不破坏 SessionReport 格式**）
- **MetricsHook 签名扩展**（公共 API 变更点）：`Arc<dyn Fn(SessionStats)>` → `Arc<dyn Fn(SessionStats, Vec<QualityFinding>)>`；`with_metrics_hook`（agent.rs:457 附近）同步；**该变更会破坏 A 的 runtime 闭包编译 → 收尾收敛（见收尾 2）**
- `runtime/lib.rs`：`attach_metrics_hook` 闭包内组装 Scorecard（stats + findings → ScoreDimensions）→ `write_scorecard` 落盘
- `serve/lib.rs`：`GET /v1/sessions/{id}/scorecard`（单份）+ `GET /v1/metrics/scorecards`（聚合，含 `overall` 趋势）

## 测试计划

| 分组 | 门禁 |
|---|---|
| A | core 单测：HookVerdict 三态；NoopToolHook 默认放行；QualityFinding serde。security 单测：no-commit-secret 命中 Blocking；PathGlob 命中；SizeLimit 命中；空 diff 零 finding。agent 集成：注册 QualityHook 写禁行区 → Deny 不执行；普通写 → after 产出 finding 进事件流；含密钥 diff → Blocking finding 且 review 短路（mock 断言 review prompt 未发送） |
| B | agent 单测：构造失败会话 → DiagnoseReport 各段非空、phases 时序单调、sub_agents 链完整；成功会话不产出。runtime 装配测试：落盘路径与留存。serve 集成：200 JSON / 404 两分支 |
| C | metrics 单测：Scorecard 公式边界（0 finding → governance 1.0；全失败 → 0.0；空会话）；list/aggregate 排序与均值。serve 集成：单份 + 聚合端点 |
| 收尾（父级） | 事件流集成测试（tool 调用 → finding → review 短路 → scorecard 落盘 → diagnose 可读）；`make check` 全量（fmt+clippy+test+doc） |

## 收尾（父级，不通过不算完成）

1. **依赖顺序**：A 完成并 `cargo check` 通过后才启动 B/C（B/C 依赖 core 新公共 API）
2. **MetricsHook 签名扩展收敛**：C 扩展签名后，修复 A 写的 `attach_metrics_hook` 闭包（runtime/lib.rs）编译错误
3. config 键 `[quality] enabled`：由 worker A 在 `config/src/lib.rs` 落地（attach_quality_hook 编译依赖），默认值 + 文档随 A 交付
4. 设计文档同步：§3.2 修正为 HookVerdict（含实现注释）；`make check` 全量通过

## 假设

- 工作区未提交改动（08-04 迭代实现：attribution/task_spec/metrics 等）保持原状，本计划不触碰、不回退
- core/runner.rs 的 `RunEvent` 加变体不破坏 WireEvent 反序列化（serde tag 判别，新变体向后兼容）
- serve 加 metrics 依赖无循环（metrics 不依赖 serve）
- agent 侧会话级 findings 收集：由 A 的 QualityHook after 事件 + agent 内部汇总（Vec<QualityFinding> 随会话结束传给 MetricsHook）
