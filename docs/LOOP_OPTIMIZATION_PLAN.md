# DeepseekNova 循环完善与低成本优化计划

> 唯一设计原则：把 DeepSeek-V4-Flash 当作一个"低成本、高频决策引擎"，而不是一个
> "一次性回答机器"。核心循环：**Observe → Plan → Tool → Verify → Reflect → Next Action**，
> 配合长上下文、动态上下文检索与工具编排。

判定标准：循环的每个阶段都必须有**明确的代码路径**与**可观测事件**；默认路径不得绕过
关键相位。

---

## 1. 现状差距摘要（代码证据）

| 循环相位 | 现状 | 证据 |
|---|---|---|
| Observe | 工具结果直接入历史，仅字符截断，无结构化观察 | `agent.rs::stream_and_process_turn` |
| Plan | 默认 Agent 无计划对象；Coordinator（LLM 规划器）是 CLI 可选参数且排除图工具 | `cli/main.rs`、`agent/coordinator.rs` |
| Tool | 17 个内置工具；但同批工具调用**串行**执行，`concurrent_tools=true` 是死配置 | `agent.rs` 的 `for call in &pending_calls` |
| Verify | 无确定性验证；B3 review 默认关、只审 diff 不验证正确性 | `config::ReviewConfig`、`agent/review.rs` |
| Reflect | 仅结束启发式沉淀（工具数/步数阈值），无 LLM 反思与计划对账 | `core/memory/engine.rs::record_task` |
| Next Action | 循环存在（工具后 `continue`），但无重规划路径 | `agent.rs` |

另外：记忆召回只在会话开始时（FTS5 关键词、top-3、200 token），无中途检索；repo map
为静态全局注入（`TODO(graph): personalized seeds`）；token 估算为 `字符数/4` 粗粒度。
这些属于后续阶段，本计划分批推进。

---

## 2. 分阶段路线图

### P1（本批次）：让默认循环的 Tool/Verify 相位完整
- 并行工具执行（读类并发、写类保序串行），让 `concurrent_tools` 配置真正生效
- 确定性 Verify 回喂：文件写入后按配置命令自动验证，失败有界回炉，超限优雅暂停
- 本计划文档（作为 PR 的一部分沉淀）

### P2：高频决策经济学
- 每步 reasoning effort 动态路由（quick 角色分类机械/关键步骤）
- 工具结果观察压缩（廉价模型产出结构化摘要替代字符截断）
- 会话内工具结果缓存（读结果按参数哈希，写后失效）

### P3：上下文与检索工程
- 真实 token 计量（中文场景偏差大）
- 中途自动检索：子目标切换与 L3 压缩后召回记忆 + 图实体
- 记忆库与代码图索引打通，嵌入检索（P2 规划落地）

### P4：产品化闭环
- Coordinator 模式补上图检索工具（清理 `TODO: coordinator graph wiring`）
- 桌面端可选计划-执行-验证视图；默认 flash/pro 角色分工模板

---

## 3. P1 详细设计

### 3.1 并行工具执行

**现状**：`stream_and_process_turn` 对同一批工具调用逐个执行；`concurrent_tools` 仅存在于
配置与模板，无消费方；`Tool::read_only()` / `ParallelSafety` 已定义但默认路径未使用。

**设计**：
1. `Agent` 增加 `concurrent_tools: bool`（默认 true），`runtime::build_agent` 从
   `config.agent.concurrent_tools` 传入——死配置变为生效配置。
2. 权限预检先行：按原始顺序对所有调用做 Allow/Deny/Ask 判定；Ask 串行等待用户，
   避免并发弹窗。
3. 分段调度：以写类工具（`read_only()==false`，未知工具按写保守处理）为分割点；
   段内只读工具用 `JoinSet` 并发，写工具独立段串行。结果按**原始调用顺序**回写，
   保证历史确定性（replay 不变量只要求结果齐全，顺序一致更利于测试与可读性）。
4. 取消语义：每个执行任务挂 `cancel.child_token()`；取消后未开始的段跳过。
5. 计数：`tool_calls_made` 按实际执行数累加，`wrote_files` 按调用名判定
   （`write_file` / `edit_file` / `move_file` / `bash`），与现状一致。

**备选方案**：
- A1 全量并发：最简单，但破坏"先写后读"的调用语义，否决。
- A2 分段并发（采用）：保序 + 并发收益，读工具多时收益最大。
- A3 仅读并发、写后读串行：等价于 A2 的特例。

**自我质疑 / 反例**：
- 同批 `write_file` + `read_file`（模型要求先写后读）→ 分段保证读在写之后执行，正确。
- 两个写工具同批 → 各自独立段，按原顺序串行，正确。
- MCP/第三方工具 `read_only` 默认 false → 保守串行；后续可让 MCP 声明 safety。
- `ParallelSafety::RequiresResource` 本批次不消费，写工具一律 Exclusive，不会并发。

### 3.2 确定性 Verify 回喂

**现状**：模型完成时直接 `Done`；B3 review 默认关闭且只审查 diff，无法验证"改完能编译/
测试通过"。

**设计**：
1. 新配置 `[verify]`：
   ```toml
   [verify]
   enabled = false      # 默认关，数据驱动后再开
   commands = ["cargo check --quiet"]   # 写入后自动执行的验证命令
   max_cycles = 1       # 失败回炉上限；超限 Paused(verify_failed)
   ```
2. `Agent` 增加 `VerifySettings`；`runtime::build_agent` 从 config 装配。
3. 触发点：模型输出 `Complete` 且本轮 `wrote_files` 时，在 B3 review **之前**执行
   （确定性验证更便宜、更客观，先跑）。
4. 执行：复用已注册的 `bash` 工具与同一 `SecurityContext`（沙箱、命令白名单、
   资源限制全部生效），逐条运行；`Ok` = 通过，`Err` = 失败（含退出码/超时/安全拦截）。
5. 反馈：失败结果以 **User 消息**注入（不能伪装成 Tool 结果——没有对应 tool_call_id
   会破坏 replay 不变量），`continue` 进入修复轮；超过 `max_cycles` 则
   `Paused(reason=verify_failed: ...)`。
6. 全部通过 → 继续原有 B3 review / Done 路径。

**备选方案**：
- B1 独立进程直跑命令：绕过沙箱与权限策略，否决。
- B2 复用 bash 工具（采用）：安全策略、超时、输出上限全部复用。
- B3 每次工具调用后都验证：成本高、噪音大，否决；只在完成前验证。

**风险与边界**：
- 验证命令不在白名单 → bash 工具返回安全拦截错误，走失败路径，有界循环不会死循环。
- 验证命令本身耗时（如全量 cargo check）→ 受 `max_execution_time` 约束，失败可回退。
- 失败信息注入需截断（上限字符），避免撑爆上下文。

---

## 4. 测试计划

- 单元：调用分段函数（保序、写分割、并发分组）；verify 成功 / 失败 / 循环耗尽三态。
- 集成（MockProvider）：同批 3 个工具调用 → 全部结果按原顺序入历史；写后 verify 失败
  → 回炉 → 再失败 → `Paused(verify_failed)`。
- 配置：`[verify]` TOML 解析与 merge。
- 全量：`make check`（fmt + clippy + workspace tests + doc）；PR CI（含 Windows）。

## 5. 禁行区（错误预扫描）

- 不改 provider 协议与 replay 不变量（`ValidatedRequest` / 压缩链）
- 不绕过沙箱、权限门、安全策略
- 不删除 L1/L2/L3 压缩链与 B3 review（仅调整执行为 verify 之后）
- 不做超出 P1 范围的架构改动（不重建 Orch/GOAP）

## 6. 置信度声明

- 现状差距：高置信度（代码直接证据）。
- P1 设计取舍：高置信度（复用既有接口，改动集中在 agent 循环与 config）。
- 优先级排序：中高置信度（默认路径体验优先于极限能力）。
