# PROMPT_DESIGN — 系统提示词分层设计

日期：2026-08-10 ｜ 原则：主 Agent 与委托子代理共享一份稳定、provider-neutral 的执行基线；角色提示词、运行时检索上下文与结构化机器契约按职责分层追加。提示词正文用英文；JSON 结构、章节名、工具清单和回复格式等机器契约保持不变。

## 共享执行基线

`crates/deepseeknova-agent/src/prompts.rs::DEFAULT_SYSTEM_PROMPT`

基线用于未配置 `[agent].system_prompt` 的主 Agent，并通过
`compose_sub_agent_prompt` 应用于每个委托子代理。它提供以下稳定约束：

- 基于仓库、配置与工具结果建立事实，不把猜测表述为事实；
- 修改前理解相关文件、调用方、测试、项目规则与成功条件；
- 复用既有模式，最小化改动，保留无关的用户变更；
- 遵守 sandbox、权限、审批、deny 规则、路径边界与秘密保护要求；
- 常规小决策自主处理；仅在会实质改变工作范围或涉及破坏性、不可逆外部操作时暂停；
- 变更后按风险执行针对性验证，并如实说明失败、跳过项与不确定性；
- 进度信息简洁且以证据为准，最终输出先给出结果；
- 只在任务真正独立且收益大于协调成本时委托子代理。

基线不包含 provider、模型、推理参数、固定工具名或逐步编排假设。它不限制并行工具调用，也不要求所有任务使用固定阶段循环。工具 schema 由运行时注入，动态召回内容不进入稳定系统前缀。

## 主 Agent 组装

主 Agent 的默认选择和覆盖语义由 `Agent::run_stream` 与 runtime 装配共同保证：

1. `[agent].system_prompt` 未配置时，首次会话注入 `DEFAULT_SYSTEM_PROMPT`。
2. 显式配置 `[agent].system_prompt` 时，配置文本完整替换主 Agent 默认值，不隐式拼接基线。
3. `Agent::with_appended_system_prompt` 在默认或显式主提示词后追加稳定运行时内容；代码图检索策略与失败模式反馈因此不会单独成为整个 system prompt。
4. repo map 仍在新会话时追加到 system 前缀；记忆召回继续作为易变的 User 消息注入，避免破坏前缀缓存。

## 委托子代理组装

委托子代理采用“共享基线 + 角色专用提示词”的组合：

1. `DelegatePreset.system_prompt` 和 `[delegate.agents].system_prompt` 表示角色说明，而不是完整执行契约。
2. `compose_sub_agent_prompt` 总是先放置共享基线，再放置非空的 `## Delegated Role` 段。
3. 父级冻结 deny 规则和参数化任务书的 `## RULES` 块继续追加在组合提示词之后；任务目标仍位于 User 消息。
4. 直接 `DelegateEngine` 路径和 coordinator 使用的 `SubAgentRunner` 路径都使用这一顺序，防止配置覆盖绕过执行和安全基线。

## 专用提示词边界

以下提示词保留独立职责，不继承通用执行基线，以避免破坏解析与协议契约：

| 位置 | 保留原因 |
|---|---|
| `plan_mode.rs::DEFAULT_PLANNING_SYSTEM_PROMPT` | 只读规划输出及其章节约束 |
| `coordinator.rs::PLANNER_SYSTEM_PROMPT` | 执行图 JSON nodes/edges 和 action 类型契约 |
| `review.rs` | 审查输入与 verdict 输出契约 |
| `compaction.rs` 与观察压缩 | 固定摘要章节与压缩语义 |
| `scanner/investigate.rs` | 安全调查的 JSON 输出字段 |
| attribution、reflection、verify 辅助请求 | 短生命周期的窄任务契约 |

## 验证边界

测试应持续证明：未配置主 Agent 使用默认值；显式主覆盖不会泄漏默认文本；图检索和失败模式提示追加在主提示词之后；子代理最终 system 消息严格按“基线 → 角色 → 冻结 deny → 渲染 RULES”排列；两条委托执行路径对 TOML 角色覆盖保持一致。
