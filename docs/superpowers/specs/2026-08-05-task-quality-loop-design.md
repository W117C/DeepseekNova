# 任务质量闭环设计：治理钩子 + 策略评估 + 结构化诊断 + 评分卡

> 日期：2026-08-05
> 状态：已实现并合入 main（ca814c1 / 2bb9909；审查修复轮 5d009f4 补充 run 级
> 差分与 session id 同源，见 §12）
> 设计依据：harness Cursor 插件（hooks.json 的 before/after 治理模式、debug-pipeline 的 structured diagnose、scorecard-review 的评分、dora-metrics/analyze-costs 的聚合指标）
> 触发协议：跨 crate 变更 + 架构级决策（core 公共 API 变更）→ 完整推理专家协议

---

## 1. 背景与动机

DeepseekNova 已有完整的执行链：permission gate（事前拦截）、verify/review/reflection（验证反思链）、SessionMetrics + CostReport（逐会话落盘）、TracingAuditLogger（审计日志）。但存在四个结构性缺口：

1. **无工具生命周期治理层**：permission gate 是"是否允许调用"的一次性裁决（struct，非可扩展链），无"调用前惯例推荐 / 调用后策略评估"的可编程钩子。harness 的 `hooks.json` 证明 before/after 双层治理是"帮 agent 不犯错"的核心模式。
2. **写后策略评估缺失**：review.rs 的 LLM 自审是唯一写后检查，所有变更（哪怕明显违规）都先烧一轮 LLM token；harness 的 `validate-policies.mjs` 证明"先确定性规则、后 LLM"能显著降本。
3. **失败诊断无结构化输出**：失败只回吐错误文本，无 stage/step 分解、时序、子代理 drill-down 的机读报告。harness `debug-pipeline`（harness_diagnose）证明单次调用拿失败全景的价值。
4. **会话指标无聚合视角**：SessionStats/CostReport 逐会话落盘但无查询/聚合 API，无法回答"agent 表现如何、钱花哪了"。harness `scorecard-review` / `dora-metrics` 证明聚合评分与趋势的价值。

四块共用同一事件流（RunEvent），数据天然贯通 → 串成"任务质量闭环"。

## 2. 现状证据（代码级，取证于 2026-08-05）

| 事实 | 证据 |
|---|---|
| permission gate 是具体 struct 非 trait | `permission/src/lib.rs:15-23`（Decision 枚举 Allow/Ask/Deny）、L29-50（Policy/Rule）、L95-160（PermissionGate，L147 check()） |
| gate 调用点在 agent 主循环 | `agent/src/agent.rs:1635` `gate.check(tool, &call.arguments)`；注入经 `Agent::with_permission_gate`（agent.rs:310），runtime 装配 |
| 工具执行在 agent 侧 | `agent/src/agent.rs:1959` `tool.execute(&ctx, &call.arguments).await` |
| Tool trait 与 RunEvent 在 core | `core/src/tool.rs:75-129`（Tool）、`core/src/runner.rs:70-121`（RunEvent 枚举）、L132-186（WireEvent，serde tag=kind）、L93（ToolResult{call_id, result: String}） |
| verify/review/reflection 在 agent crate | `verify.rs:44-52`（VerifyOutcome=Pass/Fail/Skipped）、`review.rs:24-27`（Verdict=Approve/Issues）、`reflection.rs:17-21`（Reflection{root_cause,fix_plan,lesson}）；主循环调用 agent.rs:1141（verify）/1267（review）/834（reflect_retry） |
| metrics 无查询/聚合 API | `metrics/src/lib.rs:37-52`（SessionStats）、L205-216（write_report 落盘 `<dir>/<session_id>.json`）、L197（new_session_id）；无查询函数 |
| security::policy 是白名单式 | `security/src/policy.rs:5-12`（SecurityPolicy：allowed/denied paths/commands/domains + is_*_allowed），不适合表达质量规则；模块清单 lib.rs:8-12（audit/capability/context/limits/path/policy） |
| scanner 规则是 regex struct | `scanner/src/rule.rs:37-44`（Rule{id,severity,lang,pattern:Regex,message}），非 DSL → 质量规则可参考此风格 |
| AuditLogger 真实存在 | `security/src/audit.rs:10-35`（AuditLogger trait + TracingAuditLogger） |
| serve 仅 3 端点 | `serve/src/lib.rs:98-100`（GET /health、POST /v1/chat SSE、POST /v1/approval） |
| agent 依赖 permission/security/metrics，不依赖 config | `agent/Cargo.toml:11-28`；无 config |
| runtime 装配先例 | `runtime/src/lib.rs:1150-1172`（attach_metrics_hook：读 config → 组装 → `agent.with_metrics_hook(hook)`）；同族 with_review/with_verify/with_review_counter/with_lesson_hook（agent.rs:414/457/466） |
| 上次迭代先例：core 定义 trait、agent 实现 | 08-04 设计 §3.4 AttributionHook：core 定义 hook trait 与默认空实现，agent 实现真实逻辑 |

## 3. 设计 A（阶段 1+2）：治理层 — ToolHook 链 + 写后策略评估

### 3.1 目标
工具调用前后可编程观察/建议/拦截；变更落盘后先跑 0 token 确定性规则，严重违规才升级 LLM 自审。

### 3.2 ToolHook trait（core 定义，公共 API 变更点）

跟随 AttributionHook 先例：**core 定义 trait + 默认空实现，agent 实现**。

```rust
// core/src/tool_hook.rs（新文件）
/// 工具生命周期钩子。同进程、类型化注册；panic/异常一律 fail-open（按 Allow 处理并记录）。
pub trait ToolHook: Send + Sync {
    fn name(&self) -> &str;
    /// 窄范围 bail：返回 false 的调用点不进入 before/after。
    fn interested(&self, call: &ToolCall) -> bool { true }
    /// 调用前：可拒绝（复用 permission::Decision）、可附加惯例/模板建议。
    fn before(&self, _ctx: &ToolHookCtx, _call: &ToolCall) -> Decision { Decision::Allow }
    /// 调用后：对结果做确定性评估（0 token），产出 finding。
    fn after(&self, _ctx: &ToolHookCtx, _call: &ToolCall, _result: &str) -> Vec<QualityFinding> { Vec::new() }
}

/// 无钩子注册时的默认实现（no-op）。
pub struct NoopToolHook;

// core/src/tool_hook.rs（同文件）—— finding 类型必须与 ToolHook 同层：
// core 不依赖 security，security 的 QualityPolicy 产出的是本类型。
pub enum FindingSeverity { Info, Warning, Blocking }
pub struct QualityFinding {
    pub rule: String,          // 规则 id，如 "no-commit-secret"
    pub severity: FindingSeverity,
    pub passed: bool,          // true = 规则通过（仅审计），false = 违规
    pub evidence: String,      // 命中的内容摘要/路径
}
```

**与 permission gate 的关系**（职责边界）：
- gate 是内置最后一道 `Decision::Deny` 来源，裁决"是否允许调用"（状态缓存、限流、workspace 越界防护——系统级边界）
- hook 链是调用前后可编程观察/建议/拦截扩展（含用户自定义禁行区规则——策略级边界，与 gate 的系统级边界正交）
- 决策合并：**任一 Deny → 拒绝；无 Deny 且任一 Ask → Ask；全 Allow → Allow**

**RunEvent 新增变体**（core/src/runner.rs，公共 API 变更点，WireEvent 同步）：
- `QualityFinding { rule: String, severity: FindingSeverity, passed: bool, evidence: String }`（after 钩子产出，供前端渲染）

### 3.3 写后策略评估（security crate，新模块 `quality.rs`）

```rust
// security/src/quality.rs（新文件）
/// 确定性质量规则：regex/路径/体积三类检查，0 token。
/// evaluate 产出 core::QualityFinding（类型归属见 §3.2）。
pub struct QualityRule {
    pub id: &'static str,          // 如 "no-commit-secret"
    pub severity: FindingSeverity, // 复用 core::FindingSeverity（Info / Warning / Blocking）
    pub kind: RuleKind,            // Regex{pattern, targets} | PathGlob{deny: Vec<String>} | SizeLimit{bytes}
    pub message: &'static str,
}
pub struct QualityPolicy { rules: Vec<QualityRule> }
impl QualityPolicy {
    pub fn builtin() -> Self;                       // 内置规则集
    pub fn evaluate(&self, diff: &str, changed: &[PathBuf]) -> Vec<QualityFinding>;
}
```

**叠加关系（token 降本核心）**：确定性规则先跑 → 产出 findings 进 RunEvent → 仅当存在 `Blocking` 级别 finding 时才触发 LLM review（agent.rs:1267 的 run_review_pass 前置短路）→ review 通过后正常收尾。

### 3.4 agent 侧接线

- `agent/src/quality.rs`（新）：`QualityHook` 实现（持 `QualityPolicy` + `Arc<dyn ApprovalResponder>` 可选），before 阶段可检查"写文件目标是否命中禁行区"，after 阶段对写类工具结果跑 `QualityPolicy::evaluate`
- builder：`Agent::with_tool_hook(Arc<dyn ToolHook>)`（跟随 with_lesson_hook 先例）
- runtime：`attach_quality_hook(agent, config) -> Agent`（attach_metrics_hook 范式），config 键 `[quality] enabled / rules` 走 runtime 装配

## 4. 设计 B（阶段 3）：诊断层 — 结构化失败报告

### 4.1 目标
失败时聚合"阶段分解 + 时序 + 失败详情 + 子代理 drill-down"为单份机读 JSON，TUI 与 HTTP 均可消费。

### 4.2 实现路径（已选）
**路径 1：失败时即时生成 + 落盘**（否决路径 2：serve 内存维护会话事件缓冲——事件缓冲生命周期与 serve 会话状态耦合，且 CLI 场景无 serve）。agent 主循环失败路径（reflect_retry 前，agent.rs:834 附近）收集本会话的关键 RunEvent 摘要（阶段起点/终点时间戳、工具成败、verify/review/reflection 结果、子代理链）→ 生成 `DiagnoseReport` 结构体 → **经 runtime 装配的诊断回调落盘**（attach_metrics_hook 范式：agent 不感知目录，runtime 读 `[metrics] dir` 写 `<dir>/diagnose/<session_id>.json`）。

```rust
// agent/src/diagnose.rs（新）
pub struct DiagnoseReport {
    pub session_id: String,
    pub outcome: String,                       // success | paused | failed
    pub phases: Vec<PhaseSpan>,                // plan/tool/verify/reflect：name, started_at, ended_at, duration_ms
    pub failures: Vec<FailureDetail>,          // 失败点：阶段、工具名/命令、错误摘要、归因（root_cause/fix_plan，若 reflection 已产出）
    pub sub_agents: Vec<SubAgentSpan>,         // 子代理 drill-down：preset, outcome, duration_ms
    pub quality: Vec<QualityFinding>,          // 本会话全部 finding
}
```

### 4.3 消费面
- serve 新增 `GET /v1/sessions/{id}/diagnose`（读落盘文件返回 JSON；文件不存在 → 404）
- TUI：`/diagnose` 命令读同路径渲染（低优先级，可后置）

## 5. 设计 C（阶段 4）：评分层 — 四维评分卡

### 5.1 目标
把逐会话的 verify/review/quality 结果聚合为可跨会话对比的分数，回答"agent 表现如何、趋势如何"。

### 5.2 实现（metrics crate 扩展）

```rust
// metrics/src/lib.rs 扩展
pub struct Scorecard {
    pub session_id: String,
    pub started_at_ms: u64,
    pub dimensions: ScoreDimensions,
}
pub struct ScoreDimensions {
    pub governance: f32,   // 守规：1 - blocking_findings / tool_calls（无 finding 即 1.0）
    pub verification: f32, // 验证：verifications_passed / verifications
    pub reflection: f32,   // 反思：失败路径中有 reflection 记录的比例
    pub review: f32,       // 审查：review issues 为 0 的审查轮占比
}
impl Scorecard {
    pub fn overall(&self) -> f32; // 四维均值
}
```

- 落盘：会话结束时由 metrics hook 侧组装（复用 write_report 模式），写 `<dir>/<session_id>.scorecard.json`（**独立文件，不破坏现有 SessionReport 格式**）
- 查询 API（新写）：`list_scorecards(dir) -> Vec<Scorecard>` + `aggregate(dir) -> ScorecardAggregate`（均值/趋势/最差维度）
- serve：`GET /v1/sessions/{id}/scorecard` + `GET /v1/metrics/scorecards`（聚合）

## 6. 阶段依赖与装配

```
阶段1 ToolHook（core trait + agent 接线）  ← 无依赖
阶段2 写后策略评估（security::quality）    ← 依赖阶段1的事件出口（QualityFinding）
阶段3 诊断（agent::diagnose + serve）      ← 依赖阶段1/2的 finding 数据；可并行
阶段4 评分卡（metrics 扩展）               ← 依赖阶段2的 finding；可并行
```

- 阶段 3/4 可在阶段 1 完成后与阶段 2 并行开发（文件所有权零重叠，见 §7）
- 全部收尾由父级统一验证：`make check` 全量 + 事件流集成测试

## 7. 文件所有权（并行边界）

| 阶段 | 文件 |
|---|---|
| 1 | `core/src/tool_hook.rs`（新）、`core/src/runner.rs`（RunEvent 变体 + WireEvent 同步）、`core/src/lib.rs`（导出）、`agent/src/quality.rs`（新）、`agent/src/agent.rs`（builder + 主循环挂载）、`runtime/src/lib.rs`（attach_quality_hook） |
| 2 | `security/src/quality.rs`（新）、`security/src/lib.rs`（导出）、`agent/src/quality.rs`（after 实现扩展）、`agent/src/agent.rs`（review 前置短路） |
| 3 | `agent/src/diagnose.rs`（新）、`agent/src/agent.rs`（失败路径收集）、`runtime/src/lib.rs`（诊断回调落盘，读 `[metrics] dir`）、`serve/src/lib.rs`（端点） |
| 4 | `metrics/src/lib.rs`（Scorecard + 查询）、`runtime/src/lib.rs`（scorecard 组装）、`serve/src/lib.rs`（端点） |

## 8. 验证计划

| 阶段 | 测试 |
|---|---|
| 1 | core 单测：NoopToolHook 默认放行；hook 链多钩子顺序、任一 Deny 即拒绝；interested 窄范围 bail。agent 集成：注册 QualityHook 后写禁行区文件 → before Deny；正常写 → after 产出 finding 进事件流 |
| 2 | security 单测：内置规则集对含密钥 diff 命中 Blocking；路径 glob 命中；大小超限。agent 集成：Blocking finding → review 被短路（mock 验证 review prompt 未发送）；仅 Warning → review 正常 |
| 3 | agent 单测：构造失败会话 → DiagnoseReport 各段非空、时序单调、子代理链完整。serve 集成：端点返回 200 JSON / 404 |
| 4 | metrics 单测：Scorecard 计算（四维公式边界：0 finding、全失败、空会话）；aggregate 均值。serve 集成：两端点返回正确聚合 |
| 收尾 | `make check` 全量；事件流集成测试（tool 调用 → finding → review 短路 → scorecard 落盘 → diagnose 可读） |

## 9. 风险与豁免

| 风险 | 缓解 |
|---|---|
| hook 链性能开销（每个工具调用多一次遍历） | interested 窄范围 bail 先行；默认 NoopToolHook 零成本；hook 数量上限常量 |
| hook panic 阻塞 agent | fail-open：catch_unwind → 记录 finding → 按 Allow 继续（对齐 harness fail-open） |
| 确定性规则误报阻塞任务 | 仅 Blocking 级才短路 review；规则可配置（`[quality] rules`）；误报走 finding 非 Deny（except before 阶段的禁行区 Deny 属显式策略） |
| 诊断报告收集增加主循环复杂度 | 只收集摘要（时间戳/结果/错误文本），不缓存完整事件流；生成失败不影响主流程（try 包裹） |
| scorecard 格式与现有 SessionReport 耦合 | 独立 `.scorecard.json` 文件；现有格式零改动 |
| 阶段 2 死代码（finding 无人消费） | 阶段 3/4 消费 finding（诊断 + 评分），阶段 2 交付即接线完成 |
| `make check` 全量回归 | 收尾统一执行，失败即阻塞交付 |

## 10. 范围外

- 技能 references/ 引用结构（独立低复杂度项，另立设计）
- 审计报告查询工具、PR 管理工具、混沌实验（低/高复杂度，后续迭代）
- review.rs 形式化重构（现有循环满足需求）
- config 键具体命名与文档（实现时定，随代码注释）

## 11. 验收标准（DoD）

1. 注册 hook 后：写禁行区文件被 before 拒绝；普通写工具调用产出 QualityFinding 事件（集成测试断言）
2. 含密钥的 diff 命中 Blocking 规则 → LLM review 被短路（review prompt 未发出）
3. 失败会话可产出 DiagnoseReport 并落盘，serve `GET /v1/sessions/{id}/diagnose` 返回 200 JSON
4. 会话结束落 `.scorecard.json`，四维分数与公式一致，聚合端点返回趋势
5. `make check` 通过
6. 本设计文档随实现提交同步（如有偏差，以代码注释标注）

## 12. 实现偏差记录（2026-08-05 收尾）

> 本节由知识收尾（neat-freak）追加，逐条记录实现与设计的偏差；以代码为准。

1. **before/interested panic 处理：fail-open → fail-closed**
   - 设计说法：§3.2 契约注释与 §9 风险表均为 fail-open——panic/异常按 Allow 处理并记录（catch_unwind → 按 Allow 继续，对齐 harness fail-open）。
   - 实现现状：`core/src/tool_hook.rs` 契约注释为 fail-closed——`interested()`/`before()` panic 按 `HookVerdict::Deny` 拒绝执行（warn 注明 panic 来源）；仅 `after()` panic 按空 findings 处理（fail-open，不阻断执行）。agent 主循环在 catch_unwind 内执行 interested/before，panic → Deny（F3 审查修复）。
   - 原因：before 是拦截点，panic 时放行会绕过禁行区/质量规则；安全判定 fail-closed。
2. **hook 决策与 gate 的合并方式**
   - 设计说法：§3.2 "任一 Deny → 拒绝；无 Deny 且任一 Ask → Ask；全 Allow → Allow"，hook 决策与 permission gate 取交集。
   - 实现现状：gate（系统级边界）与 tool hook 链（策略级）分别求值后合并——任一 Deny → 拒绝执行；无 Deny 且任一 Ask → 复用既有 approval 桥（`/v1/approval` / ServerApprovalResponder）等待人工裁决；全 Allow → 执行（`agent/src/agent.rs:1869-1953`）。
   - 原因：Ask 语义复用既有审批通道，不引入第二套审批路径；Deny 优先级最高保证安全边界不被 hook 放宽。
3. **诊断落盘细节（0600 + 脱敏 + suppress）**
   - 设计说法：§4.2 经 runtime 装配的诊断回调写 `<metrics dir>/diagnose/<session_id>.json`，仅约定路径。
   - 实现现状：`agent/src/diagnose.rs` 落盘前用 `redact_secrets` 脱敏（错误/命令/工具名/finding evidence 均脱敏），Unix 下 `set_permissions` 强制 0600；成功路径与取消路径调用 `suppress()` 不产报告（防止 Drop 兜底误报 outcome=failed）；未标注 session_label 时由每次 run 生成唯一 `session-<ms>-<seq>` 标注（F5/F6/F11 审查修复；`diag-<uuid>` 兜底仅保留在守卫内部，主循环路径不可达）。
   - 原因：报告含命令与错误文本属敏感数据，0600 + 脱敏为审查要求；取消是正常结束，不应产出 failed 报告。
4. **quality_findings 语义：会话累计 → run 级差分**
   - 设计说法：§4.2 DiagnoseReport.quality 为"本会话全部 finding"（会话累计）。
   - 实现现状：MetricsGuard 记录 run 起始时 findings 长度（start_len），emit 时只取 `[start_len..]` 差分切片作为本 run 新增（会话累计由 Agent 级 Arc<Mutex> 承载）；单会话上限 `MAX_QUALITY_FINDINGS = 10_000`，超限丢弃只发生在 start_len 之后（F4 审查修复）。DiagnoseGuard 与对抗审查同样按 run 起始长度切片，诊断报告/审查证据不再混入其他 run 的 findings（审查修复轮补充）。
   - 原因：并发 run 共享同一容器时会话累计会混入其他 run 的 findings；差分保证评分卡按 run 归因，上限防无界增长。
5. **MetricsHook 签名扩展（设计未提及）**
   - 设计说法：§5.2 仅说 metrics 扩展组装评分卡，未定义 hook 签名。
   - 实现现状：`MetricsHook = Arc<dyn Fn(SessionSnapshot, QualitySummary) + Send + Sync>`；`QualitySummary` 含本 run findings（差分切片）、reflection_count、review_passes、review_issues（`agent/src/agent.rs:159-177`）；runtime 在 metrics hook 侧组装四维评分卡写 `<session_id>.scorecard.json`（`runtime/src/lib.rs:1159`）。
   - 原因：评分卡需要 quality findings 与 reflection/review 计数；扩展 hook 第二参数避免参数爆炸。
6. **审查修复项（设计未覆盖，统一列示）**
   - bash 写路径启发式（F1）：`security::quality` 的 `extract_shell_write_paths` 解析 bash 命令内联写路径——重定向写敏感路径（如 .env）before Deny + after Warning。设计仅覆盖写类工具参数路径，未覆盖 bash 内联写。
   - glob 大小写归一（F2）：no-forbidden-path 匹配时双方 `to_lowercase()` 归一（原模式保持原样，仅匹配时归一），防大小写变体绕过。
   - session_label：CLI 以 `with_session_label("session-<ts>-<seq>")` 标注会话 id；serve 未显式标注时由 Agent 每次 run 生成唯一 `session-<ms>-<seq>`，评分卡/诊断/Paused 共用同一 id，serve 端点按 label 读取落盘文件（F11 + 审查修复轮；`diag-<uuid>` 兜底仅保留在守卫内部，主循环路径不可达）。设计未定义会话 id 来源。
   - 原因：均为审查阶段发现的安全/可观测性缺口。
