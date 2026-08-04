# 实现计划：Agent 执行增强（DAG 接线修复 + 失败归因重试 + 技能热更新）

> 依据：`docs/superpowers/specs/2026-08-04-agent-execution-design.md`（已批准）
> 执行模式：3 个并行 worker（文件所有权零重叠）+ 父级收尾接线 + `make check` 强制验收
> 约定：worker 只编辑自己分配的文件；工作区存在他人未提交改动（TaskSpec/SessionMetrics 实现），不得回退，须在其基础上工作

---

## Summary

修复 ExecutionGraph 的 DAG 接线断裂（依赖/条件/超时语义真实生效），为执行层加入"失败归因 → 重试/降级"机制，打通蒸馏产出 → 自动 skill 文件 → recall 注入闭环。三个方向正交，并行开发。

## 变更分组

### A. DAG 接线修复 + 并行调度（worker A）
**文件**：`crates/deepseeknova-core/src/executor.rs`、`crates/deepseeknova-core/src/graph.rs`、`crates/deepseeknova-agent/src/coordinator.rs`

- `group_into_waves` 依赖来源从 `node.depends_on` 改为从 `graph.edges` 入边推导（单一事实源）
- EdgeCondition 求值：Success 默认；Failure 边在节点失败时推进下游；节点所有入边条件未满足 → skipped（非失败），出边继续求值
- `tokio::time::timeout` 包节点执行（tokio 已有依赖），超时 → `NodeOutput::Error(Timeout)`，计入 RetryPolicy
- `Action::Parallel` 内部走 JoinSet 真并行
- 定义 `AttributionHook` trait（新公共 API，`///` 文档注释）+ `execute_node` 失败路径调用（默认 no-op）——这是与 B 的唯一接口，收尾由父级接线
- `depends_on` 字段保留、标 deprecated 注释（避免公共 API 破坏）
- `coordinator.rs` `parse_plan` 解析 `depends_on` / `parallel` / `condition`

### B. 失败归因重试（worker B）
**文件**：`crates/deepseeknova-agent/src/attribution.rs`（新）、`crates/deepseeknova-agent/src/delegate.rs`、`crates/deepseeknova-agent/src/agent.rs`

- `attribution.rs`：`Attribution { root_cause, verdict: Retry|Degrade|Abort, fix_plan }`，LLM 归因函数复用 reflection 已验证的 JSON 契约；硬预算（归因调用次数上限常量）
- `delegate.rs`：子代理失败 → 归因 → Retry（错误+root_cause+fix_plan 追加为反馈消息重新委派，受 max_retries 约束）/ Degrade（换 preset）/ Abort（上抛）
- `agent.rs`：verify/review 达 max_cycles 时，Paused 前先归因，产出带 `fix_plan` 的恢复建议
- **约束**：不得引用 core 新增 API（AttributionHook 由父级收尾接线）

### C. 技能热更新（worker C）
**文件**：`crates/deepseeknova-core/src/memory/skill.rs`、`crates/deepseeknova-agent/src/memory_distill.rs`、`crates/deepseeknova-runtime/src/lib.rs`

- `skill.rs`：`SkillManager` 支持 `.deepseeknova/skills/auto/` 子目录 + `source: distill` 强制标记；三态 draft/verified/active（注入强度递进）；清理豁免（只允许删 source=distill 且 draft，用户手写/verified/active 一律豁免）；`reload()` 方法（构造期一次性加载改为可刷新）
- `memory_distill.rs`：`DistilledKnowledge::skill` → 调用 `SkillManager.create_skill` 写 auto/（复用 `should_extract_skill` 启发式）
- `runtime/lib.rs`：会话边界触发 `reload()`

## 测试计划

| 分组 | 门禁 |
|---|---|
| A | executor 单测：含依赖图断言 wave 顺序；Failure 边失败推进下游；条件未满足 → skipped；timeout → Error(Timeout)；Parallel 真并发；coordinator parse_plan 单测 |
| B | delegate 重试单测（mock 失败 N 次后成功）；预算超限测试；Degrade/Abort 路径测试 |
| C | 集成测试：distill → auto/ 落盘（frontmatter 校验）→ reload → recall 注入；三态状态机单测；清理豁免测试（用户手写不被删） |
| 收尾（父级） | AttributionHook 接线集成测试；`make check` 全量（fmt+clippy+test+doc）通过 |

## 收尾（父级，不通过不算完成）

1. A 的 `AttributionHook` 与 B 的归因实现接线（涉及 runtime/lib.rs 或 agent/lib.rs，此时 C 已完成无并发）
2. 归因预算/质量阈值 config 键经 runtime 装配（`[metrics]` 先例；agent 不依赖 config crate）
3. `make check` 全量
4. 解决三 worker 集成编译冲突（共享工作区，对方半成品导致的编译错误由父级最终收敛）

## 假设

- 工作区未提交改动（TaskSpec/SessionMetrics）保持原状，本计划不触碰、不回退
- `tokio` workspace 依赖已满足 timeout 需求（已验证 `core/Cargo.toml` L22-24）
- planner prompt 的契约扩展不改变既有 JSON 形状（新增字段向后兼容，planner 未声明时行为不变）
- 实现代码**不自动提交**，收尾验证通过后由用户决定提交