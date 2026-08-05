# 协议增强能力包设计：协议执行引擎 + 验证强化 + 技能进化 + 失败模式库 + 度量扩展

> 日期：2026-08-05
> 状态：设计已批准（brainstorming 流程完成，用户授权直接进入多子代理并行实现）
> 设计依据：harness Cursor 插件治理模式（hooks.json before/after、check-templates 模板目录、validate-policies 策略评估）、superpowers 技能链（brainstorming → writing-plans → TDD → systematic-debugging → verification-before-completion）、DNA 五阶段工作规范（dna-spec）
> 触发协议：跨 crate 变更 + 架构级决策（core 公共 API 变更）→ 完整推理专家协议

---

## 1. 背景与动机

DeepseekNova 已有完整执行链：ToolHook 治理链（before/after 钩子 + 写后确定性策略评估 + review 短路）、verify/review/reflection 验证反思链、TaskSpec 结构化任务书、失败归因重试、SessionMetrics/Scorecard/DiagnoseReport、记忆蒸馏、技能热更新。但存在五个结构性缺口：

1. **协议无运行时强制**：DNA 五阶段（Understand→Plan→Execute→Verify→Distill）与 harness 技能链只是 prompt 文本建议，agent 可跳过、可违背，无门控、无事件、无度量。所有主流框架（Cursor/Claude Code 等）的"技能/规范"均止步于此——运行时门控是差异化空间。
2. **验证无证据锚定**：VerifyOutcome::Pass 不区分"真跑了验证命令"与"空命令直接 Pass"（verify.rs 中 `commands.is_empty()` 直接返回 Pass），无法回答"这个 done 有没有证据"。
3. **失败知识不回流**：AGENTS.md 的"错误档案管理"是人工文档，DiagnoseReport.failures 落盘后无人消费，同类失败下次照犯。
4. **技能无生命周期**：技能加载即注入，无使用频率/成功率度量，无淘汰/合并/置顶机制，技能库只会膨胀。
5. **度量无协议维度**：Scorecard 四维（governance/verification/reflection/review）缺"守规过程"维度，无法回答"agent 有多听话"。

五块共用同一事件流（RunEvent）→ 串成"协议增强能力包"。

## 2. 现状证据（代码级，取证于 2026-08-05）

| 事实 | 证据 |
|---|---|
| RunEvent::Verification 已存在 | `core/src/runner.rs:105-110`（{command, passed, summary}），WireEvent 同步（L157-162）——证据链判定可复用，**不改 VerifyOutcome** |
| VerifyOutcome 无 evidence 字段 | `agent/src/verify.rs:44-52`（Pass/Fail(String)/Skipped）；`commands.is_empty()` 直接 Pass（L70-72）且不产 Verification 事件 |
| RunEvent 无协议类变体 | `core/src/runner.rs:79-115` 全表（TextDelta/…/QualityFinding/Verification/…/Done） |
| TaskSpec 是子代理任务书非主循环计划 | `agent/src/task_spec.rs`（DelegateEngine/SubAgentRunner 消费）；主循环无结构化计划载体（PlanModeRunner 是独立只读 runner，产 TextDelta）→ drift 检测不能做计划-执行对比 |
| Skill 加载即注入，无元数据 | `skills/src/loader.rs`（SkillLoader 仅扫描+解析，无 fitness 记录）；Skill 类型在 `core/src/registry.rs` |
| FailureDetail 结构现成 | `agent/src/diagnose.rs:39-52`（phase/tool/command/error/root_cause/fix_plan）——失败模式库聚类源 |
| DiagnoseReport 落盘路径现成 | `agent/src/diagnose.rs` + runtime 装配（metrics dir/diagnose/*.json），serve `GET /v1/sessions/{id}/diagnose` 已存在 |
| Scorecard 四维现成 | `metrics/src/lib.rs`（governance/verification/reflection/review），runtime 组装落 `.scorecard.json`，serve 端点已存在 |
| QualitySummary 现成 | `agent/src/agent.rs:159-177`（findings 差分/reflection_count/review_passes/review_issues）——metrics hook 第二参数 |
| 既有先例：core 定义 trait + agent 实现 + runtime 装配 | ToolHook（core/src/tool_hook.rs）、AttributionHook、MetricsHook |
| gate 裁决复用语义 | permission::Decision（Allow/Ask/Deny）+ 既有 approval 桥（/v1/approval、ServerApprovalResponder） |

## 3. 设计 A：协议执行引擎（core 类型 + agent PhaseRunner）

### 3.1 core 新增（`core/src/protocol.rs` 新文件，公共 API 变更点）

**只做加法，不改既有签名**（保证并行 worker 编译互不干扰）：

```rust
/// DNA 五阶段。
pub enum Phase { Understand, Plan, Execute, Verify, Distill }

/// 门控违规记录（进事件流 + 评分卡消费）。
pub struct GateViolation {
    pub gate: &'static str,        // "plan-before-execute" | "verify-evidence" | "distill-on-complex" | "drift-detection"
    pub phase: Phase,
    pub severity: FindingSeverity, // 复用 core::FindingSeverity（Info/Warning/Blocking）
    pub detail: String,
}

/// 阶段迁移事件。
pub struct PhaseTransition { pub phase: Phase, pub outcome: PhaseOutcome }
pub enum PhaseOutcome { Pass, Skipped, Violated }

/// Execute 阶段 drift 检测产出（失败路径重复）。
pub struct DriftFinding {
    pub tool_family: String,   // 失败重复的工具族（如 "bash"）
    pub failures: u32,
    pub detail: String,
}

/// 协议门控 trait（core 定义，agent 实现，runtime 装配——ToolHook 先例）。
pub trait PhaseGate: Send + Sync {
    fn name(&self) -> &'static str;
    /// 对一次阶段迁移/阶段内检查求值；空 = 通过。
    fn check(&self, ctx: &PhaseGateCtx) -> Vec<GateViolation>;
}

/// 门控上下文：由 agent PhaseRunner 构造（阶段名、事件摘要、verify 配置、窗口统计）。
pub struct PhaseGateCtx {
    pub phase: Phase,
    pub verify_configured: bool,
    pub verify_passed_count: u32,     // Verification(passed=true) 事件数
    pub verify_failed_count: u32,
    pub tool_calls: u32,
    pub tool_failures_by_family: std::collections::HashMap<String, u32>, // 连续失败计数（滑动窗口）
    pub findings: Vec<QualityFinding>,
    pub has_lesson: bool,             // Distill 阶段是否已产出 lesson（memory_distill/reflection 结果）
    pub run_ms: u64,
}

/// 无门控注册时的默认实现（no-op，零成本）。
pub struct NoopPhaseGate;
```

**RunEvent 新增变体**（`core/src/runner.rs`，WireEvent 同步，QualityFinding 先例）：

```rust
PhaseTransition { transition: PhaseTransition }   // 阶段迁移：供前端渲染 + 度量
GateViolation(GateViolation)                       // 门控违规：供评分卡 protocol 维
DriftFinding(DriftFinding)                         // drift 事件：供前端渲染
```

### 3.2 内置门（agent 实现，配置 `[protocol] gates.<name> = "hard"|"soft"|"off"`）

| 门 | 判定（check 返回 violation 的条件） | 默认力度 |
|---|---|---|
| plan-before-execute | 进 Execute 前主循环无任何计划性文本产出（首轮即工具调用）→ Warning violation | soft |
| **verify-evidence** | `verify_configured=false` → 通过（未启用不罚）；`verify_configured=true` 且 `verify_passed_count≥1` → 通过；`verify_configured=true` 且零 Verification 事件（bash 缺失/取消）→ 降级通过 + Info finding；`verify_configured=true` 且 `verify_failed_count>0` 且无后续 passed → **Blocking** | **hard** |
| distill-on-complex | 工具调用 >20 的会话，Exit Distill 前 `has_lesson=false` → Warning violation | soft |
| drift-detection | Execute 内某工具族连续失败 ≥3 且其间无成功 → DriftFinding(Warning)；同会话第二次 drift → Ask 用户（是否换策略） | soft |

**裁决语义**（复用既有通道）：
- Blocking violation → 走 Deny/Ask 通道（复用 permission::Decision 合并 + /v1/approval 桥）
- Warning/Info → 事件进流 + 注入下轮 prompt 提示
- 门控 panic → fail-closed（Deny + warn），对齐 ToolHook before 语义
- 观察类门（非 Deny 路径）panic → 空结果 fail-open

### 3.3 agent PhaseRunner

`agent/src/phase_runner.rs`（新文件）：
- 维护当前 Phase + 每门调用点（阶段边界钩子，挂在主循环既有阶段节点上）
- 构造 PhaseGateCtx（会话内事件计数，复用 MetricsGuard 的差分思路：run 起始基线 + 增量）
- Execute 阶段维护滑动窗口（工具族 → 连续失败计数，窗口长度常量）
- 产出 PhaseTransition / GateViolation / DriftFinding 事件
- builder：`Agent::with_phase_runner(Arc<dyn PhaseRunnerExt>)` 或直接集成——**简化**：PhaseRunner 作为 agent 内部模块，由 runtime 经 `Agent::with_protocol_gates(Vec<Arc<dyn PhaseGate>>)` 注入门集合（跟随 with_tool_hook 先例）

### 3.4 配置（runtime 装配）

`[protocol] enabled = true`（默认 false——**默认不改变现有行为**，用户显式开启）
`[protocol] gates.<name> = "hard"|"soft"|"off"`（缺省用内置默认表）

## 4. 设计 B：验证强化（证据链 + 对抗审查）

### 4.1 证据链（复用 Verification 事件，零 verify.rs 改动）

- verify-evidence 门（见 §3.2）即证据链判定——不新增 VerifyOutcome 字段
- 会话终止判定：`verify-evidence 硬门通过 ∨ 用户显式中止`，否则 DiagnoseReport `outcome` 标注 `unverified`（diagnose.rs 只加一个字符串值，向后兼容）

### 4.2 对抗审查子代理触发

会话结束时满足**任一**条件即委派 adversarial-review 技能的子代理跑一轮（复用既有 sub_agent 路径 + budget 上限，产出写入诊断报告 `adversarial_review` 字段）：
- (a) 会话内 QualityFinding 存在 Blocking 级
- (b) 工具调用命中 security/sandbox/permission 相关路径（从 ToolResult 工具参数与 QualityFinding evidence 判断）

配置：`[protocol] adversarial_review = true`（默认 false）。子代理无 Skill 可用时优雅跳过（warn）。

## 5. 设计 C：技能自进化（deepseeknova-skills）

### 5.1 fitness 记录（`skills/src/fitness.rs` 新文件）

```rust
/// 技能使用记录（JSON 持久化，路径由调用方注入：.deepseeknova/skills/fitness.json）。
pub struct SkillFitnessRecord {
    pub skill: String,
    pub uses: u32,         // 加载次数
    pub successes: u32,    // 使用该技能的会话 outcome=success 次数
    pub failures: u32,
    pub last_used_ms: u64,
}

pub struct FitnessStore { /* 内存缓存 + JSON 落盘，容量上限 500 条，超限 LRU */ }
impl FitnessStore {
    pub fn load(path: &Path) -> anyhow::Result<Self>;
    pub fn record_use(&mut self, skill: &str, now_ms: u64);
    pub fn record_result(&mut self, skill: &str, success: bool, now_ms: u64);
    pub fn save(&self) -> anyhow::Result<()>;
    pub fn snapshot(&self) -> Vec<SkillFitnessRecord>;
}

/// 进化建议（纯函数，全部是"建议/标记"，不自动改文件）。
pub fn evaluate(records: &[SkillFitnessRecord], now_ms: u64) -> Vec<EvolutionSuggestion>;
pub enum EvolutionSuggestion {
    Deprecate { skill: String, reason: String },   // 连续 30 天未用 或 成功率 <0.3 且 uses≥5
    MergeCandidate { skills: Vec<String>, reason: String }, // 成功率曲线相近 + 描述词重叠
    Promote { skill: String, reason: String },     // 成功率 ≥0.8 且 uses≥10 → 加载顺序前移
}
```

### 5.2 消费与接线

- loader 加载时跳过 `deprecated` 标记的技能（fitness store 提供 `is_deprecated(name)`；标记持久化在 fitness.json，**不删技能文件**，可人工恢复）
- Distill 后由 runtime 调 `record_result`；技能激活时 `record_use`（hook 接线：ToolHook after 或 metrics hook 内，取会话技能引用）
- 建议输出到日志 + 诊断报告（不自动执行淘汰/合并——人工确认）
- 与 memory_distill 分工：distill 管"会话知识→记忆"，fitness 管"技能本身→元数据"

## 6. 设计 D：失败模式库（deepseeknova-security）

### 6.1 聚类与存储（`security/src/failure_pattern.rs` 新文件）

```rust
pub struct FailurePattern {
    pub key: String,               // cluster_key(phase, tool, error_hash)
    pub phase: String,
    pub tool: Option<String>,
    pub count: u32,
    pub last_seen_ms: u64,
    pub lesson: Option<String>,    // 取该簇最近 root_cause/fix_plan
}

/// 聚类键：phase + tool + 错误摘要前 64 字符的归一 hash（去除时间戳/路径/行号变化）。
pub fn cluster_key(phase: &str, tool: Option<&str>, error: &str) -> String;

pub struct FailurePatternStore { /* 内存 + JSON 落盘（.deepseeknova/security/failure-patterns.json） */ }
impl FailurePatternStore {
    pub fn load(path: &Path) -> anyhow::Result<Self>;
    pub fn ingest(&mut self, phase: &str, tool: Option<&str>, error: &str, lesson: Option<&str>, now_ms: u64);
    /// 回灌建议：按 count 降序取 top-N，拼接防错条目。N 上限 3，防 prompt 膨胀。
    pub fn suggest(&self, limit: usize) -> Vec<String>;
    pub fn save(&self) -> anyhow::Result<()>;
}
```

- 容量上限 200 条，超限按 `count` 升序（LRU）淘汰
- 数据来源：会话结束时读取本会话 DiagnoseReport 落盘文件的 failures（runtime 接线：diagnose 回调后调用 ingest）；只收集 `outcome != success` 的报告

### 6.2 回灌机制（AGENTS.md 错误档案的运行时化）

- 会话启动时 runtime 调 `suggest(3)`，注入首轮 system prompt（`## 本会话已知失败模式（自动注入）` 块，≤3 条）
- 无模式时零注入（零成本）
- 注入内容脱敏（复用 diagnose 的 redact_secrets）

## 7. 设计 E：度量扩展（deepseeknova-metrics）

### 7.1 Scorecard 协议维度 + 综合指数

```rust
// ScoreDimensions 新增字段（serde default，向后兼容）：
pub protocol: f32,   // 1 - gate_violations / phase_transitions（无违规即 1.0；phase_transitions=0 时按 1.0）
pub composite: f32,  // CompositeIndex（五维加权均值，见下）
```

- `CompositeIndex = Σ(w_i * dim_i)`，权重：governance 0.30 / verification 0.25 / protocol 0.20 / reflection 0.15 / review 0.10（权重常量可配置，默认值写死）
- 计算输入：QualitySummary（已有）扩展 `protocol_violations: u32, phase_transitions: u32`（QualitySummary 加字段，serde default）
- task_rate 指标（首次通过率/失败重试轮次）由 runtime 从 DiagnoseReport 推导后写入 scorecard 扩展字段（`first_pass: bool, retry_rounds: u32`）

### 7.2 消费面

- serve `GET /v1/sessions/{id}/scorecard` 响应体**向后兼容扩展**（新增字段可选，serde default）
- 不新增端点（诊断/评分卡端点已存在）

## 8. 阶段依赖与装配

```
阶段1 core 协议类型（protocol.rs + runner.rs 变体 + lib.rs 导出）  ← 无依赖，先行
阶段2 skills fitness / security 失败模式库 / metrics 扩展          ← 无依赖（仅依赖 core 已有类型），可与阶段1并行
阶段3 agent PhaseRunner + 证据链 + 对抗审查                       ← 依赖阶段1
阶段4 runtime 装配（[protocol] 配置 + 回灌 + 度量接线）+ serve 扩展 ← 依赖阶段1/2/3
```

**并行边界**（文件所有权零重叠）：

| Worker | 文件 |
|---|---|
| A（阶段1） | `core/src/protocol.rs`（新）、`core/src/runner.rs`（RunEvent + WireEvent 变体）、`core/src/lib.rs`（导出）、core 单测 |
| B（阶段2a） | `skills/src/fitness.rs`（新）、`skills/src/lib.rs`（导出）、skills 单测 |
| C（阶段2b） | `security/src/failure_pattern.rs`（新）、`security/src/lib.rs`（导出）、security 单测 |
| D（阶段2c） | `metrics/src/lib.rs`（Scorecard 扩展）、`metrics/src/scorecard.rs`（如文件拆分）、metrics 单测 |
| E（阶段3） | `agent/src/phase_runner.rs`（新）、`agent/src/agent.rs`（门挂载 + 对抗审查触发 + diagnose outcome 扩展）、`agent/src/diagnose.rs`（adversarial_review 字段）、agent 单测 |
| F（阶段4） | `runtime/src/lib.rs`（attach_protocol_gates + 回灌 + record 接线）、`serve/src/lib.rs`（scorecard 响应扩展）、runtime/serve 测试 |

- A 先行；B/C/D 与 A 并行（B/C/D 只依赖 core 既有类型，A 只做加法不改签名 → 编译互不阻塞）
- E/F 在 A 完成后启动（E 依赖 A 的类型；F 依赖 A+B+C+D 的产物）
- 全部收尾由父级统一：`cargo fmt` + `make check` 全量 + 事件流集成测试

## 9. 验证计划

| 层 | 测试 |
|---|---|
| core（A） | Phase 序列化；GateViolation/PhaseTransition/DriftFinding 的 WireEvent 序列化往返；NoopPhaseGate 空结果 |
| skills（B） | record_use/record_result 持久化往返；evaluate 三种建议判定（构造数据：30 天未用/低成功率/高成功高频）；容量上限 LRU；deprecated 过滤 |
| security（C） | cluster_key 对时间戳/路径差异归一；ingest 聚类计数；suggest 排序与 ≤3 上限；容量上限淘汰；JSON 往返 |
| metrics（D） | protocol 维公式边界（0 违规=1.0、全违规、无迁移=1.0）；CompositeIndex 全 0/全满/加权正确性；旧 scorecard 文件反序列化兼容 |
| agent（E） | 无计划进 Execute → Warning violation；verify 配置且零 passed → Blocking；verify passed → 通过；drift 连续失败 ≥3 → DriftFinding，第二次 → Ask；对抗审查触发条件（Blocking finding / 敏感路径）与优雅跳过 |
| runtime/serve（F） | [protocol] 配置解析（enabled 默认 false 不改变行为）；回灌 ≤3 条注入首轮 prompt；scorecard 响应含 protocol/composite 字段且旧响应体兼容 |
| 收尾 | `make check` 全量；事件流集成（阶段迁移 → violation → 评分卡 protocol 维 → 失败模式 ingest → 下会话回灌） |

## 10. 风险与豁免

| 风险 | 缓解 |
|---|---|
| 协议门控拖慢/阻断任务 | enabled 默认 false（行为零变化）；hard 门仅 verify-evidence 一个；Ask 走既有审批桥，用户可拒 |
| 并行编译互相干扰 | A 只做加法不改签名；B/C/D 不依赖 A；聚焦测试只跑本 crate（cargo test -p）；全量 make check 父级收尾 |
| core API 设计有误导致 E 返工 | 本文档类型签名决策完整，worker 照 spec 实现，禁止自行发明 |
| 失败模式回灌污染 prompt | ≤3 条上限；仅失败会话 ingest；脱敏复用 redact_secrets |
| 对抗审查子代理烧 token | 仅两个触发条件 + budget 上限 + 无 Skill 时优雅跳过 |
| fitness.json / failure-patterns.json 并发写 | runtime 单线程装配路径（attach 阶段）；文件写原子替换（tmp+rename） |
| scorecard 响应破坏前端 | 新字段 serde default；旧字段不动 |

## 11. 范围外

- 主循环结构化计划载体（drift 检测因此用失败路径版而非计划对比版）
- 自动执行技能淘汰/合并（只出建议）
- TUI 协议状态面板（可后置迭代）
- 跨会话用户画像、PR 管理工具
- 变更 AGENTS.md 错误档案的既有手工流程（回灌是补充非替代）

## 12. 验收标准（DoD）

1. core 三个新事件变体可序列化往返，NoopPhaseGate 零成本
2. `[protocol] enabled=false`（默认）时行为与现状完全一致（回归测试）
3. verify-evidence 硬门：verify 配置且零 passed → Blocking；有 passed → 通过
4. drift：同工具族连续失败 ≥3 → DriftFinding；同会话第二次 → Ask
5. skills fitness 持久化 + evaluate 三种建议 + deprecated 过滤
6. security 失败模式聚类 + suggest ≤3 + 回灌注入首轮 prompt（enabled 时）
7. Scorecard 含 protocol/composite 字段，旧文件反序列化兼容
8. 对抗审查按两触发条件委派，无 Skill 优雅跳过
9. `make check` 通过
10. 本设计文档随实现提交同步（如有偏差，以代码注释标注 + 本节追加偏差记录）

## 13. 实现偏差记录（2026-08-05 收尾，多子代理并行实现）

> 本节由父级收尾追加，逐条记录实现与设计的偏差；以代码为准。

1. **plan-before-execute 数据通道**：设计 §3.1 的 `PhaseGateCtx` 无计划文本字段。实现给 core 加了 `pub has_plan_text: bool`（只加字段不改签名），门在 check 内读该字段；PhaseRunner 侧 `set_has_plan_text` 由主循环注入（首轮用户消息后的计划性输出判定）。
2. **drift 检测基于失败路径而非计划对比**：设计 §3.2 描述"进 Execute 前无计划"与"计划-执行偏差"——实现确认主循环无结构化计划载体（TaskSpec 是子代理任务书），故 drift 门 = 工具族连续失败 ≥3（`DRIFT_THRESHOLD`）→ Warning，同会话第二次（累计 ≥2）→ Blocking（Ask 通道，无 responder 时 allow，对齐 permission Ask 既有行为）。**修正（Bugbot #3/#4/#5，2026-08-05）**：阶段级 drift 二次处理未走 Ask——Ask 桥是工具调用级裁决（permission gate 的 `ApprovalRequest` / `ServerApprovalResponder` 接线），drift 为阶段级违规无此通道；最小侵入降级为 Warning violation + 「需人工确认」标注（detail 注明），仅在 Execute transition 附加，不产 Blocking（不触发 §13 #4 的拒绝路径）。另：`DriftFinding` 事件与 `drift_reported` 计数仅在阈值**首次**跨越时 +1（窗口未清零前不重复）；`drift-detection=off` 时门从内置列表摘除，PhaseRunner 在 transition 探测门存在性以关闭计数/事件/二次附加。
3. **verify-evidence 判定复用 Verification 事件**：不改 `VerifyOutcome`（设计 §4.1 预期），`verify_configured=true` 且零 Verification 事件 → Info 降级（bash 缺失/取消），`verify_failed_count>0` 且无后续 passed → Blocking。
4. **Blocking 拒绝语义**：设计说走 Deny/Ask 通道 + 用户可见消息；实现复用既有 tool 层 `gate_block` 拒绝路径（工具结果回填 `Error: tool 'x' blocked by protocol gate`）保住 replay 不变量（不产生悬空 tool_calls），而非注入 User 消息后 return。该路径仅适用于真实 Blocking 违规（如 verify-evidence 硬门、Hard 力度提升的违规）；drift 二次处理不产 Blocking（见 §13 #2 修正），不触发本路径。
5. **unverified 报告**：设计 §4.1 的 `outcome=unverified` 仅在"协议启用 + 会话 Complete + verify-evidence 未通过"时产报告（不 suppress）；verify 通过时维持 suppress 现状。
6. **has_lesson 代理**：主循环无 memory_distill 可见性（钩子在 run_stream 层），distill-on-complex 门用 `metrics.reflection_count > 0` 代理判定。
7. **对抗审查实现**：不依赖 SubAgentRunner 注册表（agent 侧无配置入口），改为内联只读系统提示 + 独立 Agent 实例（max_steps=3、无工具注册）+ 输入/输出字符预算 cap；触发条件 (b) 用工具名/参数关键词启发式（SENSITIVE_TOOLS/SENSITIVE_MARKERS 常量），复用 diagnose 的私有 extract 函数（语义为诊断提取非敏感性判定）。开关 `Agent::with_adversarial_review(bool)`，runtime 只传开关。
8. **metrics 接线入口**：设计 §7.1 说 QualitySummary 扩展喂入——实现为 metrics 侧新增 `Scorecard::fill_protocol(violations, transitions)`（runtime metrics hook 内 compute 后覆写 protocol/composite）；QualitySummary 扩展字段由 agent 侧 MetricsGuard 填充（emit 处）。task_rate 指标（first_pass/retry_rounds）**未落地**（设计 §7.1 末条，依赖 diagnose 推导，延后迭代）。
9. **fitness record_use 未接线**：`FitnessStore::record_result` 已接线（metrics hook 按 outcome 记录）；`record_use` 留接口，CLI 传空 session_skills 时 warn 跳过（需 recall 注入侧回填技能名，延后迭代）。
10. **config 手写 Default 被 clippy 折叠为 derive**；`COMPOSITE_WEIGHTS` 类型别名 `WeightedDim` 规避 type-complexity。均为父级收尾的 clippy 修正，非语义变化。
11. **回灌/ingest/fitness 统一挂在 `[protocol] enabled=true`**（设计 §6.2/§5 未明确开关语义）；`enabled=false` 时零行为变化（回归测试覆盖）。
12. **GateViolation.gate 与 serde 派生冲突**：`&'static str` + Deserialize 派生要求 `'de: 'static`；core 手写 Deserialize（String → Box::leak），公共签名保持 spec 原样（注释说明）。**修正（父级，2026-08-05）**：`gate` 字段改为 `Cow<'static, str>`（core 测试已过），agent 侧构造点统一 `Cow::Borrowed("...")`。
13. **对抗审查触发位点与启发式收紧（Bugbot #2/#9/#12，2026-08-05）**：① 触发位点移出 unverified 分支——原实现放在 `if unverified {}` 内，verify 通过的成功会话（Blocking finding 常见场景）直接 suppress、审查永不发生；现放会话收尾（Complete 分支末尾 + 全部 Paused 终端分支：budget / verify×2 / max-steps×2，均在 `diagnose.emit` 之前注入），unverified 只影响报告 outcome。② 触发条件 (b) 启发式收紧：bash/shell 类须叠加 SENSITIVE_MARKERS（sudo/chmod/chown//etc/ 等）命中才敏感，write_file/edit_file 叠加路径 marker 才敏感，delete_file/move_file 保持无条件。③ PhaseGateCtx.findings 死输入修复：各阶段 transition 的 `build_ctx` 改传会话 quality_findings 实时快照（原为 `Vec::new()`）。
14. **Parallel 子节点结果共享（Bugbot LOW 核实 → 用户选型新语义，2026-08-05）**：审查称「Parallel 子节点并发后 Observe 读到共享快照、看不到兄弟产出」——核实确认 `execute_action` 从不写 outputs map（Observe/Reflect 只读、Parallel 仅 clone 快照），兄弟结果从未进过 map，新旧行为一致，**无回归**；但按用户选择将该行为升级为**新语义**：Parallel 作用域内创建 `Arc<RwLock<HashMap>>` 共享容器，子任务完成后写回自身结果（含失败路径），同层 Observe 优先读共享容器再回退顶层快照。`execute_action` 增 `shared: &SharedOutputs` 参数（私有函数，`None` = 非 Parallel 路径行为不变）；顶层/顺序 wave 传 `&None`。新增 2 测试：`parallel_observe_sees_sibling_tool_result`、`observe_outside_parallel_unaffected`（非 Parallel 路径回归保护）。