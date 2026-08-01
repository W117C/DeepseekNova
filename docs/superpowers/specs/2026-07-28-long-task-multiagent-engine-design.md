# Long-Task & Multi-Agent Engine — 长任务与多智能体引擎设计

- 日期：2026-07-28
- 状态：已评审（用户逐段确认 + 三点裁决与两个边界钉法已并入）
- 战略支柱：① 工作能力（对标 Codex / Claude Code：多智能体、超长任务、检查代码）
- 目标 crate：`deepseeknova-agent` / `deepseeknova-tools` / `deepseeknova-runtime` / `deepseeknova-config` / `deepseeknova-cli` / `deepseeknova-core` / `deepseeknova-store` / `deepseeknova-provider`（增量）；**删除 `deepseeknova-orch`**

## 1. 背景与现状

读码证实的核心事实：项目存在**三个实现完整、测试齐全、但零业务接线的组件**，以及一条双轨重复的多智能体路径：

| 组件 | 位置 | 现状 | 本设计的角色 |
|---|---|---|---|
| `SessionStore` | `deepseeknova-store` | JSONL 会话持久化（load/append/list/last_n），零调用；**schema 缺陷**：`StoredMessage→Message` 丢 `tool_calls`/`reasoning_content`，直接用于恢复会破坏 DeepSeek-V4 replay 契约 | B2 断点续跑落点（先修 schema） |
| `PromptBudgetController` | `deepseeknova-agent/src/budget/` | 42 行 Allow/CompressHistory/Reject，零调用 | B2 预算接线 |
| `AtomicUnitCompactor` + `HistoryUnit` | `deepseeknova-context/src/history.rs` | turn 单元化压缩器（天然尊重 must_replay），零调用 | B2 L3 压缩的选块器 |
| `orch`（GOAP+Swarm+ProgressTracker） | `deepseeknova-orch` | 实现完整+测试，**唯一编译期依赖是 desktop AppState 持有 ProgressTracker**；与 agent crate 的 Coordinator/SubAgentRunner 双轨重复 | B0 收编后裁撤 |

主循环现状差距：max_steps 到顶**报错终止**（非优雅续跑）；只有 L1（shrink_large_results 头尾截断）+ L2（slide_window 保前缀缓存），无 L3 结构化摘要；模型**无法自主 spawn 子代理**（SubAgentRunner 已接线但只有 CLI 显式入口）；无完成前自审。

## 2. 外部调研结论（决策来源）

| 来源 | 采纳 | 否掉 |
|---|---|---|
| **Ruflo**（ruvnet/ruflo，前身 Claude Flow，66k★）| "学习闭环喂规划器"（与 P1 记忆衔接）；失败从当前态重规划的**理念**（记录为规划态，B 系列不实现）；cost-tracker/hooks 思路印证 P1 方向 | Queen/mesh 拓扑 + Raft/Byzantine 共识（单机单进程过度设计，YAGNI） |
| **Claude Code** | 三层压缩架构（我们 L1/L2 已等价，补 L3 结构化摘要）；压缩后状态重建；Task-tool 式委派（描述路由、隔离上下文、结果摘要回传） | 9 段摘要裁剪为 7 段 |
| **Codex CLI** | 交接摘要（handoff）措辞风格；近窗口自动触发 | 用户消息永久保留策略（压缩效率低） |
| **OpenCode** | **压缩后重放最后一条用户消息**（近零成本、体验最好）；"剪枝收益不足不动手"阈值思想 | 时间戳隐藏式非物理删除（我们的 JSONL 会话已保留全量原文，等价审计能力） |

## 3. 决策记录

| 决策点 | 结论 |
|---|---|
| 范围 | ①委派 ②续航 ③自审三块全要，B0→B1→B2→B3 分期，每期独立可验收 |
| orch 去留 | **收编后裁撤**：ProgressTracker→core、TaskComplexity→provider（**连测试一起迁移**），GOAP"从当前态重规划"记为规划态理念；删 crate |
| 架构落点 | 方案 A：强化现有 Agent 主循环，零新 crate，P1 同款手法（钩子+扩展注入+runtime 装配）；配套拆分 agent.rs |
| 委派形态 | 单 `delegate` 工具 + 4 内置预设 + Semaphore(2) + 禁递归 + 回传封顶 |
| **信号量满** | **排队（await acquire），不拒绝**：对模型语义最简（委派=最终执行），禁递归保证无环等待；卡死由子代理 max_steps+墙钟兜底 |
| **on_max_steps** | 默认 `pause`（breaking change，CHANGELOG 标注 + `"error"` 逃生舱）；**CLI 非交互 pause 时打印 resume 提示并以退出码 3 退出**（CI 可判定不挂起）；desktop 为可续跑 UI 态 |
| **续跑autonomy** | B2 **无自动续跑**：resume 一律人工显式（`--resume`/桌面按钮），结构上杜绝"无限自动续跑烧 token"；auto-continue 留作未来项且必须自带预算 |
| **自审默认** | **默认关**（`[review] enabled=false`）：避免与 max_steps 翻转两个默认变化叠加同版；带可观测计数器，≥50 次触发数据后人工评估误报率/成本再翻默认 |
| **自审 1 轮后仍有 issues** | 标记**需人工介入并暂停**（复用 Paused 语义，reason=review_issues，会话已保存），不静默完成 |

被否方案：新建 `deepseeknova-loop` 执行引擎 crate（迁移核心热路径，churn 与回归风险最大）；Coordinator 中心化（每 run 多一次规划调用，违背支柱③）；接线 orch 为底层（维护双轨抽象）。

## 4. B0 — orch 收编裁撤

- **收编（连测试）**：`ProgressTracker` → `deepseeknova-core/src/progress.rs`（含其单元测试）；`TaskComplexity` + `to_reasoning_config` → `deepseeknova-provider`（factory 旁，含测试）。desktop `AppState.progress` 改引 core 路径。
- **依赖审计**（删除前硬性步骤）：`rg "deepseeknova[_-]orch"` 全仓确认仅剩 desktop AppState + Cargo 清单；同时核对 B1/B2 设计不引用任何将删抽象——B 系列仅依赖 agent crate 的 Coordinator/SubAgentRunner 路径，GOAP 重规划语义**不被 B2 使用**（显式记录，防裁撤后在 B2 重新发明）。
- **删除**：orch crate、workspace members、Cargo.lock；**AGENTS.md §2 权威清单同步**（单一真相源）；DESIGN.md §GOAP/§Swarm 章节改标“已裁撤，历史实现见删除提交之前的 git 历史”；CHANGELOG 记录。
- 验收：workspace 无 orch 引用；`make check` + `make check-desktop` 全绿；迁移的测试原样通过。

## 5. B1 — delegate 委派（对标 Claude Code Task tool）

- **`DelegateTool`**（`deepseeknova-tools/src/delegate.rs`）：参数 `{agent: string, goal: string, context?: string}`；经 `ctx.extensions` 取 `DelegateHandle = Arc<DelegateEngine>`（Graph/MemoryHandle 同款），缺失时友好降级文字。
- **`DelegateEngine`**（`deepseeknova-agent/src/delegate.rs`）：预设注册表 + 复用 `SubAgentRunner` + `tokio::sync::Semaphore(max_concurrent=2)`；满员**排队**；子代理输出按 `output_cap_tokens`（默认 2000，chars×4 估算）头尾截断后作为工具结果回传主循环。
- **4 内置预设**（`[delegate.agents]` 配置可增/覆盖：name/system_prompt/tools/max_steps）：
  | 预设 | 工具集 | 用途 |
  |---|---|---|
  | explorer | 只读 fs + grep/glob + 图检索三件套 + recall | 调研/定位 |
  | coder | fs 读写 + shell + 图检索 | 实现 |
  | tester | shell + fs 只读 | 跑测试/复现 |
  | reviewer | fs 只读 + grep + 图检索 | 审查 |
- **硬边界**：子代理工具集**不含 delegate（禁递归）**、不挂 DistillHook（不沉淀，防记忆污染）、不注入起点召回；继承主 agent 的 SecurityContext/PermissionGate。
- runtime 装配：`[delegate] enabled=false` 时不注册工具（graph/memory 同款 disabled 处理）。

## 6. B2 — 长任务续航

- **L3 结构化压缩**：L1+L2 后仍超阈值（或 budget 判 CompressHistory）时触发——`group_into_units` + `AtomicUnitCompactor` 选出可安全驱逐的完整 turn 单元（不碰 must_replay），交廉价模型（`[agent] compact_model`，空=主模型低 effort）产出 **7 段结构化摘要**（原始意图/关键决策/涉及文件/错误与修复/TODO/进行中/下一步，要求直引原文关键短语防漂移），经 `Memory::compact` 落回。**压缩后状态重建**：注入最近改动文件路径清单（仅路径）+ 重放最后一条用户消息。失败→退回 L2-only 现状并 warn，连败 3 次本会话停用 L3（Claude Code 同款保险）。
- **会话持久化/断点续跑**：`SessionStore` schema v2——`StoredMessage` 补 `tool_calls`/`reasoning_content`（`#[serde(default)]` 向后兼容旧文件）；runtime/CLI 接线：`[session] enabled=true`，目录 `.deepseeknova/sessions/`，每 turn append；CLI `deepseeknova sessions list` + `run/chat --resume <id>`；resume 后必须通过 `validate_replay_invariant`（reasoning 保真的验收线）。desktop 接同一机制（后续期）。会话为本地明文转写（与 history 同级敏感度），不经 redaction（记忆库才是被召回注入的面，已有脱敏）。
- **budget 接线**：`PromptBudgetController` 在 step 边界评估：`CompressHistory`→触发 L3；`Reject`→优雅停（保存会话 + 明确文字）。
- **max_steps 优雅化**：新增 `RunEvent::Paused { reason, session_id }`（core 事件枚举扩展，各前端处理）；`[agent] on_max_steps = "pause"(默认) | "error"`。pause 路径：保存会话 → 发 Paused → CLI 非交互打印 `deepseeknova run --resume <id>` 提示并 **exit code 3**；desktop 显示可续跑状态。**breaking change**：CHANGELOG 标注，逃生舱 `"error"` 保持旧行为。
- **agent.rs 拆分**（支柱④配套）：压缩逻辑拆 `agent/src/compaction.rs`，委派引擎独立 `delegate.rs`，agent.rs 不再增长。

## 7. B3 — 完成前自审（默认关）

- 触发条件（三者同时）：`[review] enabled=true` && 本轮发生文件写入（write_file/edit_file/move_file 或 shell 执行过）&& 尚未审过本轮。
- 流程：run 收尾、发 Done 前——廉价模型（`review_model`）收 `git diff --stat` + 关键 diff 片段（`diff_cap_tokens` 封顶）+ 任务文本 + 完成声明 → 宽松解析 JSON `{verdict: approve|issues, issues: [..]}`；issues → 回注反馈消息继续循环修复（**max_cycles=1**）；1 轮后仍有 issues → **Paused(reason=review_issues) + 会话已保存**，交人工。非 git 仓库或 diff 失败→跳过审查并 warn（优雅降级）。
- **可观测性**（翻转默认的数据依据）：计数 review_triggered / issues_found / fix_succeeded（memory 启用时入 counters 表，否则仅 tracing）；**翻转准则**：累计 ≥50 次触发后人工评估误报率与 token 开销再决定默认开。

## 8. 配置（全 `#[serde(default)]`，对齐 [graph]/[memory] 风格）

```toml
[delegate]
enabled = true
max_concurrent = 2
output_cap_tokens = 2000
sub_agent_max_steps = 10
# [delegate.agents] 数组可增/覆盖预设：{ name, system_prompt, tools = [..], max_steps }

[session]
enabled = true
root = ".deepseeknova/sessions"

[agent]  # 增量字段
on_max_steps = "pause"      # pause | error（旧行为逃生舱）
l3_compaction = true        # false = 仅 L1/L2 现状
compact_model = ""          # 空 = 主模型低 effort

[review]
enabled = false             # 默认关：数据验证后再翻（见 §7 翻转准则）
review_model = ""
max_cycles = 1
diff_cap_tokens = 3000
```

## 9. 错误处理与降级

全链路照搬 graph/memory 姿态：委派失败→模型友好文字（不打断 run）；L3 失败→退 L2 + warn（3 连败停用）；会话写失败→warn 不断 run；resume 文件损坏→明确报错并列出可用会话；审查任何异常→跳过并 warn。`Paused` 是**正常终态**而非错误。

## 10. 测试计划

- **B0**：迁移测试原样绿；`rg` 零 orch 残留断言（CI 可加 grep 检查）；desktop 编译。
- **B1**：delegate 往返（explorer 调研→回传≤cap）；**负例**：未知预设名、句柄缺失降级、子代理 schema 断言不含 delegate（禁递归的可测形式）、Semaphore(1) 下两个委派串行完成不失败（排队语义）。
- **B2**：schema v2 往返保 `tool_calls`/`reasoning_content` + 旧文件兼容读；**kill 进程→resume→`validate_replay_invariant` 通过**（头号验收）；L3 不吞 must_replay 单元（构造带 pending reasoning 的历史断言未被驱逐）；L3 后稳定前缀 hash 不变；max_steps→Paused 事件 + CLI exit code 3（进程级测试或 CLI 集成测试）；budget Reject→优雅停。
- **B3**：无文件写入永不触发；1 轮后仍有 issues→Paused(review_issues) 而非 Done；enabled=false 零行为变化；坏 JSON verdict→跳过 + warn。
- 验收：每期 `make check` 全绿；B0 另加 `make check-desktop`。

## 11. Token 护栏（支柱③）

子代理回传封顶 + 独立上下文（噪声不进主窗口）；L3 仅在 L1/L2 不够时触发且用廉价模型；审查默认关、开启后仅文件变更触发 1 轮、diff 封顶；budget 硬边界；无自动续跑；全部 kill-switch。

## 12. 明确不做（后续候选）

GOAP A* 目标级规划与自动重规划（理念已记录，需求出现再评估）；swarm 共识拓扑；auto-continue 自动续跑（若做必须自带预算与轮次上限）；跨机器 federation；desktop 续跑 UI（属支柱⑤，接同一 Paused/session 机制）；子代理并行写文件的冲突协调（当前靠预设工具集划分职责规避）。

## 13. 成功标准

1. **B0**：workspace 无 orch；迁移测试全绿；`make check` + `make check-desktop` 绿。
2. **B1**：一次真实 run 中模型自主完成 "delegate(explorer) 调研 → delegate(coder) 实现" 两级委派；主上下文只出现两段封顶摘要；子代理无 delegate 工具。
3. **B2**：进程被 kill 后 `--resume` 恢复且 replay 校验通过；构造超长对话触发 L3 后 run 继续、系统前缀字节不变；max_steps 到顶 CLI 以退出码 3 结束并给出 resume 命令。
4. **B3**：纯问答 run 永不触发审查；审查发现问题且 1 轮未修复时以 Paused 收场；`enabled=false` 与现状完全一致。
5. 各期开关关闭时（`[delegate]/[session]/[review] enabled=false`、`l3_compaction=false`）行为与现状逐字节一致（增量增强原则）。**唯一例外**：`on_max_steps` 默认 `pause` 是已批准的 breaking change（§3），旧行为需显式配 `"error"`。

## 14. 假设与置信度

- **置信度：高**——三个零接线组件与双轨事实均直接读码证实；B1/B3 是纯增量钩子；风险集中于 B2 触碰主循环与 `RunEvent` 枚举扩展（跨前端），靠分期 + 聚焦测试 + 逃生舱控制。
- 假设（中置信）：廉价模型足以产出可用的 7 段压缩摘要与审查判定（B3 默认关 + 计数器正是为验证此假设）；`RunEvent::Paused` 新增变体对 serve/tui 前端的改动是机械性的（实现期确认各 match 穷尽性）。
- 待实现时确认：desktop 对 Paused 的 UI 呈现（支柱⑤衔接）；`concurrent_tools` 配置下多 delegate 调用的实际并行度。
