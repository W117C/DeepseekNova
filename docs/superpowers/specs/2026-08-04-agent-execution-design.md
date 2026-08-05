# Agent 执行增强设计：DAG 接线修复 + 失败归因重试 + 技能热更新

> 日期：2026-08-04
> 状态：已实现并合入 main（实现 ca814c1；设计文档 a6743b8）
> 设计依据：harness Cursor 插件（create-pipeline-v1 的 parallel/on-failure 语义、run-pipeline 的"先归因再重试"、create-agent 的自定义 Skill 注入模式）
> 触发协议：跨 crate 变更 + 架构级决策 → 完整推理专家协议

---

## 1. 背景与动机

DeepseekNova 框架已具备：4 种 Runner、DelegateEngine（子代理并发）、TaskSpec（结构化任务书）、verify/review 回炉循环、reflection 归因原语、ExecutionGraph、记忆蒸馏与 SkillManager。深度代码审查发现三处真实缺口：

1. **DAG 接线断裂**：`ExecutionGraph` 的并行图执行脚手架约 80% 已存在，但 `depends_on` 全代码库无赋值 → 所有节点落入 wave 1 并发乱序执行；`EdgeCondition` 从不求值；`timeout` 字段从未使用。框架声称支持并行图执行，实际是并发乱序。
2. **执行层无失败归因**：只有 provider API 层重试（`deepseeknova-provider` retry.rs）与节点级盲重试（同参重放）；DelegateEngine 子代理失败直接上抛，无"归因 → 重试/降级"机制。
3. **技能生成链路断裂**：`SkillManager.create_skill`（写 `.md` skill 文件）无任何运行时调用方；蒸馏（`memory_distill`）产出只落记忆库，不生成可热载入的 skill 文件。

## 2. 现状证据（代码级）

| 事实 | 证据 |
|---|---|
| `depends_on` 仅声明+读取，无赋值 | `core/src/graph.rs` L44/L54（声明+`Vec::new()`）；`core/src/executor.rs` L307-334（读取）；全库 grep 无写入 |
| `EdgeCondition` 从不求值 | 仅 `graph.rs` 出现 3 次（定义），executor/coordinator 无引用 |
| wave 分组只看 `depends_on` | `executor.rs` `group_into_waves` L318-361 → 空依赖时全部节点进 wave 1 |
| 拓扑排序基于 edges 有效 | `executor.rs` `topological_sort` L276-314（edges → in_degree） |
| `timeout` 字段未使用 | `graph.rs` ExecutionNode 定义，executor 无 timeout 包装 |
| 节点级盲重试存在 | `executor.rs` L190-213，同参重放，无归因 |
| reflection 归因原语已验证 | `agent.rs` L818 `reflect_retry`：`{root_cause, fix_plan, lesson}` JSON 契约，verify/review 回炉前调用 |
| `SkillManager.create_skill` 无运行时调用方 | `core/memory/skill.rs` L160-171；runtime 只把蒸馏产物落记忆库（`runtime/lib.rs` L626-646） |
| agent 不依赖 config crate | `agent/Cargo.toml` L23 仅依赖 core；预算/阈值配置需走 runtime 装配（先例：`[metrics]` 配置经 `attach_metrics_hook`） |
| core 已有 tokio（workspace） | `core/Cargo.toml` L22-24 → timeout 修复零新依赖 |

## 3. 设计 A：DAG 接线修复 + 并行调度

### 3.1 目标
让 ExecutionGraph 的依赖/条件/超时语义真正生效，planner 产出的图按依赖顺序分波并行执行。

### 3.2 实现路径（已选）
**路径 1：executor 内部从 edges 推导依赖**（否决路径 2：`add_edge` 同步维护 `depends_on`——条件边无法用 `Vec<NodeId>` 表达，且改动面扩散到所有调用点）。

- `group_into_waves` 的依赖来源从 `node.depends_on` 改为**由 `graph.edges` 推导**（入边集合即依赖集，单一事实源）
- `depends_on` 字段保留（避免公共 API 变更），标注 deprecated 注释
- **EdgeCondition 求值**：节点完成后按出边条件决定下游触发：
  - `Success`：节点成功 → 下游进入待执行
  - `Failure`：节点失败 → 下游仍进入待执行（失败处理路径，harness on-failure 语义）
  - `Retry` / `ToolCall`：按条件匹配触发
  - **默认条件 = Success**：planner 未声明条件时所有边照旧触发（向后兼容，现状行为不变）
- **skipped 语义**：节点所有入边条件均未满足 → 节点跳过（skipped，非失败），其出边按条件继续求值
- **timeout 生效**：节点执行包 `tokio::time::timeout`（tokio 已有依赖），超时 → `NodeOutput::Error(Timeout)`；与 RetryPolicy 交互：超时计入重试（超时即失败）
- **`Action::Parallel` 真并行**：内部子 action 走 `JoinSet`

### 3.3 planner 契约扩展
`coordinator` 的 `parse_plan` 增加解析：`depends_on`（节点级）、`parallel` 组、`condition`（边级）。对应 planner prompt 模板同步更新（CoordinatedPlanMode 提示词）。映射 harness create-pipeline-v1 的 `parallel` / `on-failure` 语义。

### 3.4 归因注入点预留（与设计 B 的接口）
`execute_node` 失败路径预留 `AttributionHook`（trait，默认 no-op）：
- `core` 定义 hook trait 与默认空实现（公共 API 变更点，需文档注释）
- `agent` 实现真实归因逻辑，收尾接线为**强制验收项**（见 §6）

## 4. 设计 B：失败归因重试（agent 层）

### 4.1 目标
子代理/图节点失败时，先归因再决定重试/降级/放弃，而非盲重试或直接上抛。

### 4.2 实现路径（已选）
**路径 1：反馈追加式重试**（否决路径 2：新建独立归因-重试状态机——全新代码无实战验证）。复用 verify 回炉已验证的模式：重试 = 把错误 + root_cause + fix_plan 追加为反馈消息重新委派。

### 4.3 组件
- **`agent/src/attribution.rs`（新文件）**：`Attribution { root_cause, verdict: Retry | Degrade | Abort, fix_plan }`，JSON 契约基于 reflection 已验证的 `{root_cause, fix_plan, lesson}` 扩展
- **DelegateEngine 失败重试**：子代理失败 → 归因 → `Retry`（追加反馈重试，受 max_retries 约束）→ `Degrade`（换 preset / 降并发）→ `Abort`（上抛）
- **agent 主循环失败路径**：verify/review 达 max_cycles 时，Paused 前先归因，产出带 `fix_plan` 的恢复建议（Paused 恢复可续用）
- **硬预算**：归因调用次数上限（防烧 token），超限走盲重试/直接 Abort。预算默认值先为 agent 内常量，config 键走 runtime 装配（见 §6.3）

## 5. 设计 C：技能热更新

### 5.1 目标
蒸馏/reflection 产出的经验可自动生成 skill 文件并进入 recall 注入闭环。

### 5.2 实现路径（已选）
**路径 1：会话边界热重载**（否决路径 2：落盘即时热重载——需 RwLock 化 SkillManager 共享状态，风险高）。热重载延到会话边界触发。

### 5.3 组件
- **独立命名空间（安全关键）**：自动生成 skill 写入 `.deepseeknova/skills/auto/` 子目录（与用户装载的 superpowers skills 隔离），frontmatter 强制 `source: distill` 标记
- **质量门槛三态**（语义明确：注入强度递进）：
  - draft：落盘后初始态，**低优先级**试用注入（仅匹配度高时），`record_use` 统计效果
  - verified：`record_use` 达标（阈值常量）后转正，**常规** recall 注入
  - active：verified 且跨会话存活 N 次，**长期保留**，清理完全豁免
- **清理豁免规则（安全关键）**：自动清理（LRU/阈值）**只允许**作用于 `source: distill` 且状态为 draft 的 skill；用户手写、verified、active 一律豁免，绝不删除
- **热重载**：`SkillManager::reload()`，runtime 在会话边界（或蒸馏落盘后）触发；构造期一次性加载改为可刷新
- **范围外（可选）**：deepseeknova-skills crate（skill 即工具）与 SkillManager（skill 即记忆上下文）的双表面同步——本设计只做 SkillManager 侧闭环，双表面打通列为后续专项

## 6. 并行开发边界与集成收尾

### 6.1 文件所有权（零重叠）

| Worker | 文件 |
|---|---|
| A（DAG） | `core/src/graph.rs`、`core/src/executor.rs`、`agent/src/coordinator.rs` |
| B（归因） | `agent/src/attribution.rs`（新）、`agent/src/delegate.rs`、`agent/src/agent.rs` |
| C（技能） | `agent/src/memory_distill.rs`、`core/memory/skill.rs`、`runtime/lib.rs` |

### 6.2 收尾强制验收（父级执行，不通过不算完成）
1. A 预留的 `AttributionHook` 与 B 的归因实现**接线**
2. 配置装配：B 的归因预算、C 的质量阈值 config 键（agent 不依赖 config，经 runtime 装配，先例 `[metrics]`）
3. `make check` 全量通过（fmt + clippy + test + doc）

### 6.3 依赖关系
- B 不依赖 A 的 hook 实现，只依赖其接口签名；收尾接线由父级完成
- C 不依赖 A/B；A 不依赖 B/C
- 全部三 worker 完成后父级统一验证

## 7. 验证计划

| 设计 | 测试 |
|---|---|
| A | executor 单测：含依赖图断言 wave 顺序；EdgeCondition::Failure 边在失败时推进下游；入边条件未满足 → skipped；timeout 超时 → Error(Timeout) 且计入重试；`Action::Parallel` 真并发 |
| B | delegate 重试单测（mock 子代理失败 N 次后成功）；归因预算超限测试；Degrade/Abort 路径测试 |
| C | 集成测试：distill → `auto/` 落盘（含 frontmatter）→ reload → recall 注入；三态状态机单测；清理豁免测试（用户手写 skill 不被删） |
| 收尾 | `make check`；hook 接线集成测试 |

## 8. 风险与豁免

| 风险 | 缓解 |
|---|---|
| 条件边语义变化引入竞态/结果不一致 | 默认条件 = Success（向后兼容）；skipped 语义显式定义；依赖修复本身**降低**并发乱序风险（现状全并发 → 有序） |
| LLM 归因不可靠/烧 token | 硬预算 + 盲重试兜底；归因失败走 Abort 不阻塞 |
| 自动生成的 skill 污染 recall 上下文 | `auto/` 命名空间隔离 + `source: distill` 标记 + 三态门槛 + 清理豁免 |
| B 死代码（hook 未接线） | §6.2 强制验收项 |
| `make check` 全量回归 | 收尾统一执行，失败即阻塞交付 |

## 9. 范围外

- 设计 B 的 config 键具体命名与文档（收尾时定）
- deepseeknova-skills 与 SkillManager 双表面打通
- SessionMetrics 报表/聚合扩展（已实现，无新工作）
- review 形式化重构（现有循环已满足需求，增量价值有限）

## 10. 验收标准（DoD）

1. 构造含 `depends_on`/`condition` 的图，执行顺序符合依赖（单测断言）
2. 子代理失败 → 归因 → 重试成功路径可复现
3. 蒸馏产出 skill 落 `auto/`、reload 后 recall 可注入、用户手写 skill 不受清理影响
4. `make check` 通过
5. 本设计文档随实现提交同步（如有偏差，以代码注释标注）
