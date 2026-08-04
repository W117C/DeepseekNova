# DeepseekNova Agent 效能度量（SessionMetrics）设计

- 日期：2026-08-04
- 状态：设计完成，待实现
- 范围：新增 `deepseeknova-metrics` crate（数据模型 + 采集聚合 + 落盘），`deepseeknova-agent`（MetricsHook 注入 + 事件采集点），`deepseeknova-runtime`（装配 collector + 组装报告落盘）。跨 3 crate + 1 新 crate，属架构级变更，实现需 `make check` 全绿
- 设计依据：harness Cursor 插件 `dora-metrics` skill（效能指标 + 报表），映射到 DeepseekNova 现有数据面

## 背景与现状

DeepseekNova 已有三个碎片化数据面，但**会话级聚合与调优报告完全缺失**：

| 已有 | 覆盖 | 缺口 |
| --- | --- | --- |
| `MeteredProvider` / `CostLedger`（provider crate） | run 级 token + USD 成本 | 无会话维度、无执行面指标 |
| `RunEvent` 流（core） | 流式事件（ToolCallStart/End、Verification、Usage） | 事件是流不是聚合；工具**失败**无结构化事件（被折叠进 ToolResult 文本 `"Error: ..."` 前缀） |
| OTLP tracing（telemetry） | span 级链路 | 默认关闭；面向监控系统，非调优报告 |

目标：会话级效能指标（完成率/失败率/重试率/步数/工具成功率/成本），run 结束自动落盘，支撑 prompt 与配置调优。

## 方案选型

| 方案 | 结论 |
| --- | --- |
| A：新 crate `deepseeknova-metrics` + agent 内部 hook 采集（**选定**） | 成败面在 agent 内部是结构化的（`execute_tool_call` 的 Result），采集准确；采集模型/聚合/落盘独立可测；hook 对称 `distill_hook` 注入模式 |
| B：agent crate 内 SessionStats + runtime 落盘 | 少一个 crate，但报告组装/落盘塞进已 1784 行的 runtime；agent 职责继续膨胀 |
| C：包装 RunEvent 流消费 | 零侵入 agent，但工具失败检测靠 `"Error: "` 文本前缀解析 —— **脆弱**（工具输出本身可能含此前缀），per-tool 失败计数不可靠，否决 |

## 数据模型（新增 `deepseeknova-metrics` crate）

```rust
/// run 内的局部累加器（同步、非共享）。每个 run_stream 实例化一个，
/// run 结束 snapshot 后经 hook 传出。run 隔离天然成立（局部变量），
/// 并发 run 同一 Agent 实例互不污染（DelegateEngine 并发场景安全）。
#[derive(Debug, Clone, Default)]
pub struct SessionTracker { /* 内部计数 */ }

impl SessionTracker {
    pub fn new() -> Self;
    pub fn observe_step(&mut self);                         // 主循环迭代
    pub fn observe_tool_call(&mut self, name: &str, ok: bool); // 结构化成败
    pub fn observe_retry(&mut self);
    pub fn observe_verify(&mut self, passed: bool);
    pub fn mark_outcome(&mut self, outcome: RunOutcome);
    pub fn snapshot(&self) -> SessionStats;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    Completed,        // 正常完成（工具调用后无进一步调用）
    PausedMaxSteps,   // max_steps 到顶（优雅暂停）
    Cancelled,        // 取消令牌触发
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SessionStats {
    pub started_at_ms: u64,          // tracker::new() 时的系统时间戳（ms）
    pub duration_ms: u64,            // snapshot() 时计算（now - started_at）
    pub steps: u64,
    pub tool_calls: u64,
    pub tool_failures: u64,
    pub tool_failures_by_name: HashMap<String, u64>,
    pub tool_calls_by_name: HashMap<String, u64>,
    pub retries: u64,
    pub verifications: u64,
    pub verifications_passed: u64,
    pub outcome: Option<RunOutcome>,   // run 结束前为 None
}

/// 落盘报告：执行面 + 成本面。
#[derive(Debug, Clone, Serialize)]
pub struct SessionReport {
    pub session_id: String,            // 时间戳 + 随机后缀，runtime 生成
    pub stats: SessionStats,           // 含 started_at_ms / duration_ms
    /// 成本为**累计**语义（CostLedger 为 Agent 级共享、跨 run 累计，
    /// 与 CLI 总成本一致）；per-run diff 列为后续增强。
    pub cost: CostReport,              // 复用 provider::CostLedger::report
}
```

**为什么放弃流式事件、改局部累加器**：Agent 是共享实例（`Arc<Agent>`），`DelegateEngine` 信号量允许并发 run 同一 preset 的 Agent；若采集经共享 hook 流式回调，并发 run 会互相污染。`SessionTracker` 作为 run_stream 内的局部变量，run 隔离天然成立，无需 Mutex/run_id。且 `RunOutcome` 需 run 结束才可知，一次性 snapshot 语义更自洽。

**成本为何不进 tracker**：成本已有 `CostLedger`（Agent 级共享，MeteredProvider 注入），run 结束时 runtime 直接 `ledger.report(&prices)` 取。注意其累计语义已在上方标注。

## Hook 注入（agent crate）

```rust
// deepseeknova-agent/src/agent.rs —— 对称 distill_hook
pub type MetricsHook = Arc<dyn Fn(SessionSnapshot) + Send + Sync>;

// Agent 字段：metrics_hook: Option<MetricsHook>
// builder：with_metrics_hook(hook) -> Self
```

采集方式：run_stream 内实例化局部 `SessionTracker`，各采集点同步 `tracker.observe_*()`，run 结束（Done 事件发送前）`tracker.mark_outcome(...)` 并 `hook(tracker.snapshot())` **只调用一次**。

采集点（agent 内部，全部**已有的结构化信息**，无新增解析）：

| 采集点 | 调用 |
| --- | --- |
| 主循环 `for step in 0..max_steps` 顶部 | `observe_step()` |
| `execute_tool_call` 返回处 | `observe_tool_call(name, ok)` |
| `reflect_retry` 调用处 | `observe_retry()` |
| verify 执行处（P4 确定性验证） | `observe_verify(passed)` |
| run 结束（Done 事件发送前） | `mark_outcome(...)` + `hook(snapshot())` |

**outcome 判定**：主循环正常退出（工具调用后无更多调用）→ `Completed`；`for` 循环耗尽（max_steps 到顶）→ `PausedMaxSteps`；取消令牌触发 → `Cancelled`。hook 在 `Done` 事件发出前调用一次。

**hook 调用方的并发义务**：Agent 只保证"每次 run 恰好调用一次 hook、传入该 run 的 snapshot"。若调用方（runtime）需要跨 run 聚合，自行负责隔离（如按 session_id 分文件落盘）；Agent 不承担共享状态。

## Runtime 装配与落盘

```rust
// deepseeknova-runtime/src/lib.rs
// build_agent / runtime 启动时：如果 config.metrics.enabled，为 Agent 注入
// metrics_hook。闭包捕获 Arc<CostLedger> + PriceTable + 输出目录：
//   |snapshot| {
//       let report = SessionReport {
//           session_id: format!("{}-{}", ts, rand),
//           stats: snapshot,                // 含 started_at_ms / duration_ms
//           cost: ledger.report(&prices),   // 累计语义
//       };
//       metrics::write_report(&report, &dir);  // 失败仅 warn!，不阻断
//   }
// run 结束（run_stream 消费完成）时 Agent 恰好调用一次 hook。
```

- 落盘目录：`.deepseeknova/metrics/`（与 graph.db / memory.db 对称）；目录不存在则创建
- 文件格式：`<ISO时间戳>-<rand>.json`，单个文件一个 SessionReport（文件名含随机后缀，并发 run 写各自文件，天然隔离）
- 文件写入失败仅 `warn!` 不阻断 run（度量是辅助，非关键路径）
- **config 开关**：`[metrics] enabled`（默认 true，与 delegate 一致；用户可关）。config crate 加 `MetricsConfig`

## 依赖方向

```
deepseeknova-metrics  →  provider（CostReport / CostLedger）
deepseeknova-agent    →  metrics（SessionTracker / RunOutcome / SessionSnapshot）
deepseeknova-runtime  →  metrics（落盘 writer）
deepseeknova-config   →  MetricsConfig（独立，无新依赖）
```

无循环依赖。工作区 `Cargo.toml` members 加 `crates/deepseeknova-metrics`。

## 测试计划

| 层 | 测试 |
| --- | --- |
| metrics crate 单测 | tracker 累加正确性（步数/成败/重试/验证/outcome）、空 tracker 快照、SessionReport JSON 序列化往返、write_report 落盘（临时目录） |
| agent crate | 注入 hook 后 run 结束恰好调用一次、snapshot 计数正确（MockProvider 驱动）、无 hook 时零行为变化（旧测试原样通过）、outcome 判定（Completed/PausedMaxSteps/Cancelled） |
| runtime | 装配后 run 结束生成报告文件（临时目录）、`[metrics] enabled=false` 不注入 hook 不落盘 |
| config crate | `[metrics] enabled` 解析 |

## 验收标准

1. `make check` 全绿
2. 无 metrics_hook 时 Agent 行为零变化（既有测试原样通过）
3. run 结束后 `.deepseeknova/metrics/` 生成 JSON 报告，含执行面 + 成本面
4. `[metrics] enabled=false` 不落盘
5. 新 crate 零新外部依赖（serde/serde_json/thiserror 项目已有）