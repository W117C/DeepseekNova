# PROMPT_DESIGN — 全链路系统提示词统一设计

日期：2026-08-01 ｜ 原则：DeepSeek-V4-Flash 是低成本高频决策引擎；一切提示词共享
Observe → Plan → Tool → Verify → Reflect → Next Action 六阶段词汇；提示词用英文；
机器契约（JSON 结构、章节名、工具清单、回复格式）一律不变，只统一角色定位与协议措辞。

## 主提示词（任务 2 已落地）

`crates/deepseeknova-agent/src/prompts.rs::DEFAULT_SYSTEM_PROMPT`

结构：Identity → Operating Principle（决策引擎）→ The Loop（六阶段协议）→
Action Discipline → Tool & Retrieval Rules → Verification & Reflection →
Cost & Context Care。工具 schema 不写入，由 `context::PromptBuilder` 运行时注入。
接入：`Agent` 无配置时默认注入（agent.rs run_stream）；`with_appended_system_prompt`
在无配置时 = 默认 + 追加；config 覆盖仍优先。

## 子提示词改动清单

| # | 位置 | 现状 | 改动 | 保留契约 |
|---|---|---|---|---|
| 1 | plan_mode.rs DEFAULT_PLANNING_SYSTEM_PROMPT | 泛化规划助理 | 定位为 Plan 阶段；输出契约显式化 | 5 个章节名不变 |
| 2 | coordinator.rs PLANNER_SYSTEM_PROMPT | 泛化规划助理 | 首行点明 Plan 阶段 | JSON nodes/edges、action 类型、示例逐字保留 |
| ~~3~~ | ~~coordinator.rs PLANNER_SYSTEM_PROMPT_GOAL~~ | ~~Goal Mode 规划~~ | ~~首行点明 Plan 阶段~~ | ~~同上~~（2026-08-08 已随 goal_mode 死代码删除，见 AUDIT M2b） |
| 4 | delegate.rs 4 预设 | 角色一行无阶段定位 | 每个预设标注所在阶段（explorer=Observe、coder=Tool、tester=Verify、reviewer=Reflect）+ 输出契约 | 工具清单与角色名不变 |
| 5 | review.rs render_review_prompt | 泛化审查者 | 定位 Reflect 阶段 | `# Task`/`# Completion claim`/`# Diff`、JSON verdict 指令不变 |
| 6 | compaction.rs render_l3_prompt | 泛化压缩 | 定位为循环的记忆压缩阶段 | 7 个 `##` 章节名逐字保留 |
| 7 | scanner investigate.rs build_prompt | 泛化安全审查 | 定位 Verify 阶段 | `true_positive`/`note` JSON 指令、Rule/File/excerpt 占位符不变 |
| 8 | runtime GRAPH_RETRIEVAL_HINT | 中文检索策略 | 英文化，语义不变 | 检索优先级（图工具 > grep/整读）与三个工具名 |
| 9 | agent.rs compress_observation | 内联英文压缩提示词 | 提取为 render_compression_prompt，定位 Observe 阶段 | 保留事实/路径/退出码/数字的指令、纯摘要输出 |
| 10 | agent.rs verify 失败回炉文案 | 内联固定文案 | 提取为 verify_failure_message（语义已符合循环，仅固化+测试） | `[verification failed]` 标记与「修复后重跑验证」语义 |

## 为什么这样统一（决策记录）

- 六阶段词汇统一后，主 agent 与子代理/规划器/审查器在同一协议语言下协作，模型在
  delegate/verify/review 之间切换时不需要重新理解角色。
- 机器契约逐字保留：planner 的 action 类型、review/scanner 的 JSON 回复、compaction
  的 7 章节是解析器与测试的硬依赖，改词不改结构。
- 英文：与既有 LLM 提示词（delegate/review/compaction/planner）一致，token 省、跨模型稳定；
  中文只保留在面向人的注释与文档。
- verify 失败回炉文案未改语义（本就是「修复 → 重跑验证」循环），仅提取为可测函数。
