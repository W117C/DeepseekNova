# DeepseekNova 对标 Claude Code / Codex — 差距分析与优化路线图

> 日期：2026-08-12　｜　类型：架构级优化计划（含差距分析）　｜　状态：待评审
> 本文档是**调研 + 优化计划**，不是代码变更。落地方案前需按 AGENTS.md §1.1 完整协议评审。

---

## 0. 摘要（一页结论）

**目标**：让 DeepseekNova 在能力、架构、性能、真实任务完成率上达到甚至超越 Claude Code 与 Codex，
同时把 token 消耗压到最低。

**调研结论（三条主线）**：

1. **功能/安全面我们已经领先**：权限门控（allow/ask/deny/hard_deny）、只读命令四层分类器、
   路径双重守卫（词法归一 + 符号链接 canonicalize）、三平台沙箱、QualityPolicy 0-token 确定性规则、
   ToolHook 链、四维评分卡、失败诊断、持久化 runs、会话检查点——这些确定性强制能力超过
   Claude Code（它明确承认"提示词规则不可靠，必须用 hook/权限"）。
2. **token 效率与提示词缓存是我们最大的结构性短板**：Claude Code 把提示词缓存当作架构核心
   （静态优先、动态最后、四层缓存分层、缓存命中率告警 SEV），我们只有 `CacheAwarePromptBuilder`
   的**前缀 hash 诊断**，Anthropic 请求路径**没有注入 `cache_control` 断点**，缓存命中率没有
   落地为指标与告警，工具集/会话变化没有缓存稳定策略。这是单次收益最大的方向。
3. **上下文压缩停留在"够用"而非"最优"**：我们有 L1 截断 + L2 滑动窗口 + L3 LLM 摘要三级链，
   但压缩在主线上下文中进行（无 fork 缓存调用）、压缩前不阻塞写工具、摘要注入形态有
   （`AtomicUnitCompactor` 丢弃 reasoning signature）可疑点，且不保留"最近消息 + 摘要"的
   Codex 式结构。

**路线图分三档**：
- **P0（立即可做，低风险纯收益）**：Anthropic `cache_control` 断点注入；usage 记账修复
  （reasoning_tokens 硬编码 0 / 重复发射）；OpenAI 残缺 tool call flush 修复 + 显式 max_tokens；
  Anthropic body-read 零输出重试；工具 schema 缓存复用（消除每步 tool_map 重建）；前缀稳定性
  审计；缓存命中率指标 + 阈值告警。
- **P1（结构性，需设计评审）**：压缩链升级（写前阻塞 / fork 摘要 / 最近消息保留 / 修 AtomicUnitCompactor）；
  MCP `defer_loading` 桩；Plan Mode 工具集稳定化；子代理文件化（`.deepseeknova/agents/*.md`）
  + 嵌套深度提升 + hand-off 消息；recall 注入预算化；对标 eval 基准建设。
- **P2（长期演进）**：会话级三层缓存分层；按模型动态 token 预算；压缩边界长任务恢复；
  端到端性能基准；缓存命中率进 OTel/serve 聚合。

**度量口径**：缓存命中率 ≥90%（CC 实践目标）、同一 eval 任务集优化前后 input token 总量、
完成轮数、TTFT/每步延迟、评分卡四维分。

---

## 1. 调研方法与可信度

| 来源 | 方式 | 可信度 |
|------|------|--------|
| 本项目 22 crate 源码（agent/core/context/provider/tools/security/sandbox/metrics/skills/checkpoint/serve/config 等） | 直接阅读关键文件 + 行号核对 | 高（代码级事实） |
| `AGENTS.md` / `DESIGN.md` / `GUIDE.md` / `BUILDING.md` / `Cargo.toml` | 直接阅读 | 高 |
| Claude Code 官方博文《Lessons from building Claude Code: Prompt caching is everything》(2026-04) | 联网 | 中-高（官方工程实践自述） |
| OpenAI《Unrolling the Codex agent loop》+ 第三方 Codex 内核分析 | 联网 | 中（含版本差异风险） |
| DeepSeek API 文档（token 换算 0.3/0.6、prompt cache hit/miss） | 代码注释 + 已知文档 | 中（cache_control 在 anthropic 兼容端点的实际行为需实测） |

> **错误预扫描**：本报告不声称 CC/Codex 的内部实现细节为逐字节事实（公开资料无源码级证据）；
> 涉及 DeepSeek 端点对 `cache_control` 的实际支持度，标注为"需实测"，不当作已确认能力。

---

## 2. 现状能力基线（真实水平盘点）

### 2.1 功能面

| 能力 | 现状 | 代码位置 |
|------|------|----------|
| 主 agent 循环 | Observe→Plan→Tool→Verify→Reflect→Distill 六阶段，max_steps 默认 25 | `agent/loop_impl.rs:151-1074` |
| 双模型协调器 | planner（只读工具）+ executor 图执行，含 think/reflect/delegate/parallel 节点 | `agent/coordinator.rs` / `core/executor.rs` |
| 子代理 | `SubAgentRunner` + `DelegateEngine`，4 内置预设（explorer/coder/tester/reviewer），并发 2，深度 3 | `agent/sub_agent.rs` / `agent/delegate.rs` |
| 工具 | 16 内置工具（fs/grep/shell/glob/ls/memory/todo/web_fetch/web_search/lsp/图检索/Context7 docs），schema 总预算 6700 字符 | `tools/src/lib.rs:121` |
| 记忆 | 四层：短期 / SQLite FTS5 任务记忆（召回+蒸馏） / 技能 md / 用户画像 | `core/memory/` |
| 代码图 | tree-sitter 多语言 + SQLite FTS5 + PageRank + repo map（默认 1024 token 预算） | `graph/` |
| 权限/安全 | allow/ask/deny/hard_deny、只读四层分类、危险形态硬拒、路径双守卫、能力门、速率限制、审计日志 | `permission/` + `security/` |
| 沙箱 | macOS Seatbelt / Linux bubblewrap / Windows JobSandbox，三档（ReadOnly/WorkspaceWrite/FullAccess） | `sandbox/` |
| 质量闭环 | ToolHook 链、QualityPolicy 确定性规则（0 token）、LLM review（B3 默认关）、对抗审查（协议默认关）、失败诊断、四维评分卡 | `core/tool_hook.rs` / `security/quality.rs` / `metrics/` |
| MCP | stdio/HTTP 客户端、发现、工具适配 | `mcp/` |
| HTTP API | `/v1/chat` SSE、`/v1/sessions*`、`/v1/runs`+resume、`/v1/approval`、评分卡/诊断端点 | `serve/src/lib.rs:453-478` |
| 其他 | LSP 编辑后诊断、Auto 模型+思考路由、检查点回滚、技能 fitness 生命周期、OTLP 可选、CLI eval 门禁 | 各 crate |

### 2.2 每轮固定 token 成本（估算，实测口径为 0.3 EN / 0.6 CJK）

| 项目 | 估算 token | 说明 |
|------|-----------|------|
| `DEFAULT_SYSTEM_PROMPT`（主/子代理共享执行契约） | ≈1,100–1,300 | `prompts.rs:13-75`，7 段英文 |
| 16 工具 schema 序列化 | ≈1,700–2,000 | 6700 字符预算，经 `ToolSchemaCache` 按地址缓存 payload |
| repo map | ≤1,024 | `GraphConfig.repo_map_tokens` 默认 |
| 项目上下文（DEEPSEEKNOVA.md / AGENTS.md） | 不定 | `ProjectMemory` 注入前缀 |
| 子代理附加：角色提示 + 冻结 deny + RULES | ≈150–300 | `delegate.rs:100-169` + `sub_agent.rs:562-570` |
| Coordinator planner 系统提示 + 工具列表 | ≈1,000–1,300 | `coordinator.rs:105-160` |

**结论**：一次缓存 MISS 的固定前缀成本约 **3,900–4,600 token**（主 agent 路径），每次请求重发；
缓存命中时按 DeepSeek 缓存价计费。**把固定前缀稳定化、命中率最大化是 token 优化的第一杠杆**。

### 2.3 主循环每步开销（代码级事实）

1. `memory.get_all()` 每步全量克隆快照（A1 已提供 `iter_all` 零拷贝，但请求路径仍走克隆）`loop_impl.rs:328`。
2. 每步重建 `tool_map`（`schema().name` 每工具每步一次），绕过 `ToolSchemaCache` `loop_impl.rs:490-493`。
3. `ValidatedRequest::new` 每次 provider 调用前跑 replay 不变量校验（O(n) 扫描）`provider/lib.rs:126-144`。
4. 压缩链（L1 截断 → L2 滑动窗口 → L3 LLM 摘要）在 turn 末执行 `loop_impl.rs:422-487`。
5. 工具调度：读工具并发（JoinSet）、写工具串行；只读工具结果会话缓存（写后整体清空）`loop_impl.rs:1646-1766`。
6. 每步 provider 选择：整轮 auto-router 决策 > 每步 effort_routing（quick/high）> 默认 `loop_impl.rs:497-507`。

### 2.4 已知代码级缺陷（调研中发现，P0 候选）

| # | 缺陷 | 位置 |
|---|------|------|
| 1 | Anthropic 路径 `reasoning_tokens: 0` 硬编码，deepseek-anthropic 端点系统性漏报推理成本 | `anthropic.rs:661-671` |
| 2 | Anthropic 流式 usage 每 `message_delta` 重复发射，一轮多个 Usage 事件 | `anthropic.rs:851-857` |
| 3 | OpenAI `flush_pending_tool_calls` 只 flush id+name 齐备项，args-only 残缺项被静默丢弃 | `openai.rs:542-560` |
| 4 | Anthropic 流式重试无 body-read 阶段保护（断流丢尾） | `anthropic.rs:359-378` |
| 5 | OpenAI 从不写 `max_tokens`；`ChatCompletionRequest.max_tokens` 是死代码 | `openai.rs:148-172` / `types.rs:22-24` |
| 6 | `AtomicUnitCompactor` 用 Tool 消息承载 reasoning_content 但丢弃 signature，语义混乱，重放有 400 风险 | `context/history.rs:370-383` |
| 7 | 三个 prompt builder 并存（`PromptBuilder` / `CacheAwarePromptBuilder` / `OrderedPromptBuilder`），段序不一致 | `context/lib.rs:207/304/501` |
| 8 | `PrefixShape` 死代码（全仓无调用点） | `core/types.rs:120-203` |
| 9 | 续聊恢复时若 store 无 System 消息则跳过系统提示注入，上下文漂移 | `agent/mod.rs:1045-1072` |
| 10 | `ScavengeStateMachine` 无生产调用点（闲置代码） | `provider/scavenge.rs` |

---

## 3. 对标基准：Claude Code / Codex 关键机制（公开资料）

### 3.1 Claude Code（Anthropic 官方工程实践，2026-04 博文）

1. **提示词缓存是架构核心**：系统提示词按"静态优先、动态最后"组织——
   `静态 system prompt + 工具定义`（全局缓存）→ `CLAUDE.md`（项目内缓存）→ `会话上下文`（会话内缓存）→ `对话消息`（增长段）。
   缓存命中率是运维指标，过低会触发 SEV。
2. **缓存稳定纪律**：静态提示词不放时间戳；工具顺序确定性；**会话中途不增删工具集**
   （Plan Mode 用 `EnterPlanMode`/`ExitPlanMode` 工具切换，而非换工具集）；MCP 工具用
   `defer_loading` 桩（只发名字 + `defer_loading:true`，模型选中才加载全 schema），
   保持前缀逐字节稳定。
3. **压缩不破缓存**：上下文满时 fork 一个缓存调用去总结对话，然后用摘要替换原消息继续。
4. **技能预算重注入**：压缩时按预算重注入 invoked skills，超预算丢弃最旧的。
5. **子代理 markdown 化**：`.claude/agents/*.md`（YAML frontmatter：name/description/model/tools
   + body 系统提示词）；嵌套 5 层；**只回最终消息 + 元数据**，中间过程不进父上下文；
   用子代理做模型间 hand-off。
6. **确定性规则优先**："Never do this" 类规则用 hook/权限强制，不依赖提示词。

### 3.2 Codex（OpenAI 官方《Unrolling the Codex agent loop》+ 内核分析）

1. **ReAct 循环**：推理 → 工具调用 → 输出回填 → 再推理，直到 assistant 消息；错误回填后由
   模型自驱动恢复，无显式 orchestrator 重试。
2. **上下文管理**：全历史每轮重发，静态前缀 + 动态后缀；缓存命中时成本近似线性。
   缓存 miss 触发源：工具可用性变化 / 模型切换 / 沙箱重配置 / 审批模式 / cwd 变化。
3. **自动压缩**（约 95% 窗口，180K–244K token）：全历史送 `/responses/compact` 专用摘要端点，
   服务端返回 **AES 加密摘要 blob**；压缩前**阻塞写工具**；重建 = 初始提示 + 最近 ~20K token
   用户消息 + 加密摘要；失败指数退避重试。
4. **内核级沙箱**：Linux Landlock / macOS Seatbelt 由自引用二进制重入强制，`ToolOrchestrator`
   按审批模式 + 风险分类选沙箱。
5. 大窗口（GPT-5.4 1M token）缓解压缩频率，但压缩精度仍会随次数下降。

### 3.3 与我方逐项对照

| 机制 | Claude Code | Codex | DeepseekNova 现状 | 差距 |
|------|-------------|-------|-------------------|------|
| 提示词缓存 | 核心架构 + 命中率 SEV | 静态前缀 + 自动缓存 | 前缀 hash 诊断；Anthropic 无 `cache_control` 断点；无命中率指标 | **大** |
| 工具集稳定性 | 中途不换集；MCP defer_loading | 缓存 miss 触发源之一 | 会话内工具集基本稳定；MCP 工具全量注入无桩 | 中 |
| 压缩 | fork 缓存调用摘要，不破缓存 | 服务端加密摘要 + 最近消息保留 + 写前阻塞 | L1/L2/L3 本地链；主线内摘要；写前不阻塞 | 中 |
| 子代理 | markdown 文件化，嵌套 5，只回最终消息 | 集中工具编排 | 4 内置预设（代码硬编码），深度 3，输出 cap 2000 token | 中 |
| 确定性安全 | hook/权限强制 | 内核级沙箱 | readonly 分类器 + 路径双守卫 + 三平台沙箱 + QualityPolicy | **领先** |
| 质量闭环 | 无同构物 | 无同构物 | 评分卡/诊断/review/对抗审查 | **领先** |
| 可观测性 | 缓存命中率告警 | 无公开同构物 | scorecard/diagnose/runs/OTLP 可选 | 中 |

---

## 4. 差距分析（按收益排序，每条含现状证据、目标、多路径探索、反例）

### 4.1 提示词缓存（P0 主战场，收益最大）

**现状证据**：
- 主 agent 只在会话开始时构建一次系统前缀并作为首条 System 消息入 memory
  （`agent/mod.rs:1049-1072`），此后每轮全量重发——前缀**文本上稳定**，这是好基础。
- 但 provider 层：`anthropic.rs` 的 `build_request` 构造 `system` 字段与 `tools` 数组时
  **没有任何 `cache_control` 断点**（grep `cache_control` 仅命中 types.rs 的
  `prompt_cache_hit_tokens` 计费字段）；`openai.rs` 仅靠 DeepSeek 服务端自动缓存
  （`stream_options.include_usage` 拉取 hit/miss 计数）。
- 有 `CacheAwarePromptBuilder` + prefix hash（`context/lib.rs:329-393`）与
  `PromptCacheStabilityTracker`（`context/lib.rs:582-631`），但只在**诊断/告警**层面
  （hash 变了打 warn），不参与请求体构造。
- 无缓存命中率指标落盘/上报；`CostLedger` 记了 `cache_hit_tokens`/`cache_miss_tokens`
  （`provider/cost.rs:126-135`）但未派生命中率指标。

**目标**：前缀缓存命中率 ≥90%；命中时每次请求省 ~3,900 token 的输入成本。

**多路径探索**：
- **路径 A（显式断点）**：在 Anthropic 请求的 `system` 块与 `tools` 数组末尾注入
  `cache_control: {"type":"ephemeral"}`。收益：对支持断点的端点显式声明缓存边界；
  风险：deepseek-anthropic 兼容端点是否识别该字段未知 → **需实测**（先用一条
  双请求实验：同前缀两次调用，比对 `prompt_cache_hit_tokens` 是否 >0）。
- **路径 B（零代码，纯顺序纪律）**：不注入断点，仅保证前缀逐字节稳定（工具顺序确定性
  + 静态段无时变内容），依赖 DeepSeek 自动缓存。收益：零风险；风险：无法控制断点位置，
  大工具数组可能把缓存段撑得过大或拆散。
- **路径 C（A+B 组合，推荐）**：注入断点 + 顺序纪律 + 命中率指标。断点做成可配置
  （`provider` 配置项，默认开启但允许关闭），实测不支持时自动回落路径 B。
- **反例/失效边界**：会话中途工具集变化（MCP 加载/卸载、plan mode 切换）会整体破坏前缀缓存
  ——因此 4.3 的工具集稳定化是 4.1 的必要配套；`\n\n---\n\n` 分隔符本身不破坏缓存，
  但**任何段内容变化**（如 repo map 重建、DEEPSEEKNOVA.md 被编辑）都会 miss，需在
  命中率指标上可见。

### 4.2 上下文压缩（P1）

**现状证据**：turn 末压缩链 `shrink_large_results → slide_window → try_compact`（L3 为 LLM
结构化摘要，熔断 `MAX_STRIKES=3`）`loop_impl.rs:422-487`；摘要以 `[Compaction digest]` Tool
消息打头 `memory.rs:131-139`；`AtomicUnitCompactor` 把 ToolExchange 替换为带
`reasoning_content` 但**丢弃 signature** 的 Tool 消息 `context/history.rs:370-383`。

**差距点**：
1. L3 在主线上下文中跑（摘要调用本身消耗主线 token），CC 是 fork 缓存调用、Codex 是
   服务端独立压缩——我们**没有"独立小上下文摘要"路径**。
2. 压缩前不阻塞写工具（Codex 明确阻塞）。
3. 摘要后不保留"最近消息 + 摘要"结构（Codex 保留 ~20K 最近用户消息）。
4. `AtomicUnitCompactor` 的 Tool+reasoning 形态违反 replay 语义（丢 signature，
   若该 reasoning 需回放则 400）。
5. 压缩阈值默认 `max_memory_tokens=32_000`（`budget/controller.rs:23-29`），对 128K 窗口
   偏保守，可压缩频率过高。

**多路径探索**：
- 路径 A（对齐 Codex）：压缩时 fork 一个**独立 Provider 调用**（同一 system 前缀，
  messages 只含历史摘要请求），产出摘要后以最近消息 + 摘要重建上下文；写工具先阻塞。
  收益：主线上下文不再被摘要调用污染；风险：多一次 API 调用（成本 ~0.5–1K token 输出），
  需在低优先级任务上验证净收益。
- 路径 B（最小改动）：保留主线 L3，但修复 `AtomicUnitCompactor` 形态（摘要改 User 角色、
  不克隆 reasoning）、压缩前阻塞写工具、压缩后保留最近 N 条消息。
  收益：改动小、风险低；上限低于 A。
- **推荐**：先 B 后 A（B 立即可做，A 作为 P1 迭代）。
- **反例**：fork 摘要的独立调用若不带完整历史摘要则丢失关键决策链；必须把此前所有摘要
  （或摘要链）纳入 fork 输入，否则信息丢失随压缩次数累积（Codex 自述"多次压缩后精度下降"）。

### 4.3 工具集稳定化与 MCP defer_loading（P1）

**现状证据**：16 内置工具在会话内稳定注册；但 **MCP 工具**经 `mcp/adapter.rs` 全量注入
（无桩机制）；`plan_mode.rs` 的 PlanModeRunner 使用 `DEFAULT_PLANNING_SYSTEM_PROMPT`
（`plan_mode.rs:79-93`）——需核实 plan mode 是否切换工具集（若是，则破坏缓存）。

**目标**：前缀中的工具数组在会话内**逐字节不变**；MCP 工具以 `defer_loading` 桩形式存在，
选中才拉全 schema。

**多路径探索**：
- 路径 A（对齐 CC）：给 MCP 工具适配层加桩（name + `defer_loading:true`，DeepSeek
  OpenAI 兼容端点是否支持 `defer_loading` 需实测；不支持则退化为"按需裁剪但保持顺序"）。
- 路径 B：把"工具集快照"纳入会话配置，plan mode/子代理模式切换时**不增删工具数组**，
  只注入行为指令（CC 的 EnterPlanMode 模式）。
- **反例**：如果端点不支持 `defer_loading`，桩会变成"模型调用一个不存在的完整工具"→
  必须实测端点行为后再决定桩 vs 全量。

### 4.4 子代理（P1）

**现状证据**：4 内置预设代码硬编码（`delegate.rs:100-169`）；深度 `DEFAULT_MAX_DEPTH=3`
（`sub_agent.rs:38`）；输出经 `cap_output` 头尾截断（`delegate.rs:527-563`）；子代理独立
上下文、只回最终文本（已对齐 CC 的核心模式）。

**差距点**：用户不可自定义角色（CC 是 markdown 文件）；深度 3 vs 5；无 hand-off 消息模式
（模型切换/子代理间交接）；无"多子代理并行编排结果不进父上下文"的脚本级中间态（CC 的
orchestration 中间结果在 script variables）。

**多路径探索**：
- 路径 A（文件化对齐 CC）：`.deepseeknova/agents/*.md`（frontmatter: name/description/
  model/tools + body 系统提示词），加载进 `SkillResolver` 同源解析；深度 3→5。
- 路径 B（配置化演进）：沿用 `TaskSpec` + `[delegate.agents]` 配置扩展预设，不做文件格式。
- **推荐**：A（与 skills 加载器复用，工作量可控，对齐行业生态）。
- **反例**：文件化必须校验 frontmatter 非法字段/路径穿越（agent 文件是提示词注入面，
  沿用 security 的 sanitize 与能力裁剪）。

### 4.5 记忆与技能注入预算（P1）

**现状证据**：recall 注入 volatile User 消息（`tools.rs:11-28`），无显式 token 预算；
压缩后 `inject_recall`（`loop_impl.rs:469-485`）；skills fitness 生命周期已实现
（`skills/fitness.rs`：uses/successes/failures + deprecate/merge/promote 建议）。

**差距点**：CC 压缩时对 invoked skills 做预算重注入（超预算丢最旧）；我们压缩后只注入
recall，不重注入会话内用过的技能上下文；recall 块大小无上限（可能反噬上下文）。

**目标**：recall 注入按 `max_recall_tokens` 预算裁剪；压缩后按预算重注入 invoked skills。

### 4.6 运行性能细节（P0 代码级）

见 §2.4 缺陷表。其中 #2/#3/#5 直接影响正确性（usage/工具调用丢失），#1/#6/#7 影响
成本与可维护性，均为 P0 候选。

### 4.7 质量闭环与可观测性（P1 深化）

现状已是差异化优势（评分卡/诊断/对抗审查/QualityPolicy）。缺口：
- 缓存命中率指标（§4.1）未进评分卡/报告 → 建议 `CostLedger` 派生 `cache_hit_rate` 写入
  Scorecard 新增维度或 metrics 报告字段。
- 压缩频率/精度无观测（Codex 自述压缩精度随次数下降）→ 建议 diagnose 记录压缩事件
  （次数、触发阈值、摘要长度）。

### 4.8 无需改动（确认保持领先）

只读分类器、路径双守卫、沙箱三档、QualityPolicy 确定性规则、ToolHook 链、评分卡——
这些已超过 CC/Codex 的公开能力，保持并继续用回归测试守护。

---

## 5. 优化路线图（分档、含验证方式）

> 每项标注：动机 / 改动位置 / 涉及 crate / 验证方式 / 风险与反例。
> 全部跨 crate 变更按 AGENTS.md §1.1 走完整推理专家协议；改动后统一 `cargo fmt` + `make check`。

### P0 — 立即可做（低风险纯收益，不改变行为语义）

| # | 项 | 改动位置 | 验证方式 | 风险/反例 |
|---|----|----------|----------|-----------|
| P0.1 | **Anthropic `cache_control` 断点注入**：`system` 块与 `tools` 数组末尾加 `{"type":"ephemeral"}`，做成 provider 配置项（默认开，可关） | `provider/anthropic.rs` `build_request` / `AnthropicRequest` 结构；`config` ProviderConfig 加字段 | ① 集成测试断言请求体含 `cache_control`；② 双请求实测 `prompt_cache_hit_tokens>0`；③ 命中率指标 ≥90% | deepseek-anthropic 端点可能忽略该字段——实测不支持则默认关（路径 B 纪律兜底） |
| P0.2 | **usage 记账修复**：Anthropic 流式从 `message_delta` 的 usage 取 `reasoning_tokens`（对齐 OpenAI 的 `completion_tokens_details`），去掉硬编码 0；usage 事件去重（只发最终一次） | `provider/anthropic.rs:661-671, 851-857` | 集成测试断言 reasoning 记账非 0；stream 测试断言 Usage 事件唯一 | 某些端点不出 reasoning 明细→保持 0 且不 panic |
| P0.3 | **OpenAI 残缺 tool call flush**：`flush_pending_tool_calls` 补 args-only 项（id+args 齐备即 flush，缺 id 丢弃并告警） | `provider/openai.rs:542-560` | 流式测试：args 先到、id 后到的顺序回放 | 极端下仍可能丢项（协议本身无解）→ 告警不静默 |
| P0.4 | **Anthropic body-read 零输出重试**：流式 body 阶段若零 chunk 则按 `retry_with_backoff` 重试一次（对齐 OpenAI） | `provider/anthropic.rs:359-378` | 模拟断流测试：第一轮零输出、第二轮成功 | 不能对已输出部分重试（防重复）——只在零输出时重试 |
| P0.5 | **OpenAI 显式 max_tokens**：从 config `context_window` 或新配置推导（默认 8192），激活死代码 `ChatCompletionRequest` | `provider/openai.rs:148-172` / `provider/types.rs` | 请求体断言含 `max_tokens`；长输出不被静默截断 | 模型最大输出 < 配置值会 400——用保守默认 |
| P0.6 | **工具 schema 缓存复用**：`loop_impl.rs:490-493` 每步 `tool_map` 重建改走 `ToolSchemaCache` 快路径 | `agent/loop_impl.rs` / `provider/tool_cache.rs` | 单测：同一步两次取 map 只 build 一次；bench 对比 | schema() 本身开销小，收益有限但零风险 |
| P0.7 | **前缀稳定性审计**：① 静态提示词无时变内容（时间戳/随机/版本号）检查；② 工具顺序确定性（已有 `tool_schema_serialization_is_order_deterministic` 测试兜底）；③ 统一 `PromptBuilder` 与 `CacheAwarePromptBuilder` 段序（tools 在 repo_map 前的漂移） | `context/lib.rs` / `agent/mod.rs` | 前缀 hash 两次构建一致测试；grep 静态段中的 `Utc::now` / `rand` | 改动段序可能影响既有行为→先测试后改 |
| P0.8 | **缓存命中率指标**：`CostLedger` 派生 `cache_hit_rate`，写入会话报告与评分卡；CLI 结束摘要打印；阈值（如 <60%）打 warn | `provider/cost.rs` / `metrics/lib.rs` / `agent/loop_impl.rs` | 单测：构造 hit/miss 桶断言命中率；端到端 run 后报告含指标 | 无 usage 的 unmetered 调用不参与分母（已处理） |
| P0.9 | **死代码清理**（顺手）：`PrefixShape`（core/types.rs）、`ScavengeStateMachine`（provider/scavenge.rs）确认无调用点后删除或标注 | `core/types.rs` / `provider/scavenge.rs` | `cargo check` + grep 零引用 | 删除前用 `find_references` 复核 |

### P1 — 结构性优化（需设计评审，跨 crate）

| # | 项 | 改动位置 | 验证方式 | 风险/反例 |
|---|----|----------|----------|-----------|
| P1.1 | **压缩链升级（先 B 后 A）**：B=修 `AtomicUnitCompactor` 形态（摘要改 User 角色、不克隆 reasoning）+ 压缩前阻塞写工具 + 压缩后保留最近 N 条消息；A=fork 独立 Provider 调用做摘要（同 system 前缀，输出 ~0.5–1K token） | `context/history.rs:370-383` / `agent/memory.rs` / `agent/compaction.rs` / `agent/loop_impl.rs` | ① 压缩后 replay 不变量仍通过；② 摘要质量对比（保留关键决策链断言）；③ fork 摘要前后 input token 总量对比 | fork 调用增加一次 API 成本；摘要链必须传此前摘要防信息累积丢失 |
| P1.2 | **MCP `defer_loading` 桩**：适配层把 MCP 工具以桩（name + `defer_loading:true`）注入，选中才拉全 schema；端点不支持则按 4.3 路径 B 退化 | `mcp/adapter.rs` / `provider/openai.rs` / `provider/anthropic.rs` | 端点实测桩行为；工具被调用时 schema 正常展开的集成测试 | 桩被调用但展开失败→fail-closed 报错而非静默 |
| P1.3 | **Plan Mode 工具集稳定化**：核实 plan mode 是否切换工具集；若是，改为指令切换（Enter/ExitPlanMode 模式）保持前缀稳定 | `agent/plan_mode.rs` / `agent/coordinator.rs` | 前缀 hash 在 plan mode 进出前后不变测试 | 若 plan mode 只注入指令已稳定→仅补测试 |
| P1.4 | **子代理文件化**：`.deepseeknova/agents/*.md`（frontmatter: name/description/model/tools + body），复用 skills 解析器；深度 3→5；hand-off 消息模式（子代理返回结构化交接文本） | `agent/delegate.rs` / `agent/sub_agent.rs` / `agent/task_spec.rs` / `skills/` | 文件加载/覆盖/校验测试；嵌套 5 层集成测试；hand-off 文本契约测试 | agent 文件是提示词注入面→frontmatter 校验 + sanitize + 能力裁剪（沿用现有安全机制） |
| P1.5 | **recall 注入预算化 + 压缩后技能重注入**：recall 按 `max_recall_tokens` 裁剪；压缩后按预算重注入 invoked skills（超预算丢最旧，对齐 CC） | `agent/tools.rs` / `agent/memory.rs` / `agent/compaction.rs` / `skills/fitness.rs` | 注入大小断言测试；压缩后技能上下文存在性测试 | 预算过小会丢关键记忆→默认值取实测中位数 |
| P1.6 | **对标 eval 基准建设**：建 `evals/` 任务集（20–50 例，覆盖：单文件修改/多文件重构/跨语言/调试/长会话/压缩边界），每例含 must_contain、dimension_min、cost_max、rounds；`deepseeknova-cli eval` 已有门禁能力，扩展 token/轮数/命中率字段 | `crates/deepseeknova-cli/src/eval.rs` / `evals/` 新目录 | 优化前后跑同一基准，报告：通过率 / 综合分 / 总 token / 轮数 / 命中率 | 任务集过小会过拟合→任务按领域分层、留 20% 盲测 |
| P1.7 | **续聊系统提示补注**：seed 后检测首条非 System 消息时补注系统前缀（修漂移） | `agent/mod.rs:1045-1072` | 旧 store 无 System 消息的恢复测试 | 双注风险→检测已有 System 才跳过 |

### P2 — 长期演进（对齐前沿，需独立设计）

| # | 项 | 说明 |
|---|----|------|
| P2.1 | 会话级三层缓存分层（global/project/session） | 对齐 CC 四层结构：全局规则（AGENTS.md）→ 项目记忆 → 会话上下文逐层稳定，静态段只变一次 |
| P2.2 | 按模型动态 token 预算 | 128K 固定 → 读 provider `context_window` 动态推导 `max_memory_tokens`/压缩阈值 |
| P2.3 | 压缩边界长任务恢复 | `SessionCheckpoint` + `runs resume` 深化：压缩摘要 + 检查点恢复中间态（对齐 Codex 加密摘要理念的本地实现） |
| P2.4 | 端到端性能基准 | criterion 扩展 agent 级基准：TTFT、每步延迟、tokens/sec、工具并发吞吐 |
| P2.5 | 缓存命中率进 OTel/serve 聚合 | trace 指标 + `/v1/metrics` 聚合端点（对齐 CC 命中率 SEV 实践） |

---

## 6. 度量与验收：如何证明"达到甚至超越"

### 6.1 指标集（每个 P0/P1 合入后必须可测）

| 指标 | 口径 | 目标 |
|------|------|------|
| 前缀缓存命中率 | `cache_hit / (cache_hit + cache_miss)`（CostLedger 派生） | ≥90%（CC 实践目标） |
| 每任务 input token 总量 | eval 基准统计（含缓存命中前/后两口径） | 优化后较基线下降（目标 ≥30%） |
| 任务完成率 | eval 通过率（must_contain + dimension_min） | 与基线持平或提升 |
| 完成轮数 | eval rounds 均值 | 不劣化（目标持平或下降） |
| 压缩质量 | 压缩后 replay 不变量通过率 + 关键决策链保留断言 | 100% 通过 |
| 单步延迟 / TTFT | 端到端基准（P2.4） | 建立基线，逐步优化 |

### 6.2 验收流程

1. 每个 P0 项：聚焦测试 + `make check` 通过（按 AGENTS.md §4 约定）。
2. 每个 P1 项：设计评审（含反例清单）→ 实现 → 聚焦测试 → 同一 eval 基准前后对比。
3. 每两周一跑 P1.6 基准，把通过率/成本/轮数/命中率四表归档到 `evals/results/`。
4. 云端安全审查（若可用）覆盖 provider/security 相关改动；不可用时按 AGENTS.md
   回退路径留存 `make check` + `make audit` 证据。

### 6.3 反例与失效边界总清单（落地前逐条审）

- cache_control 在 deepseek-anthropic 端点的实际行为（P0.1 前置实测）。
- 压缩 fork 调用若不带历史摘要链 → 信息累积丢失（P1.1）。
- 子代理文件化是提示词注入面 → sanitize + 能力裁剪必须同步（P1.4）。
- 工具集/段内容任何变化都会破坏前缀缓存 → 命中率指标可见（P0.8）。
- usage 去重若误删未合并的累计值 → 记账偏小（P0.2 用最后一次完整 usage）。

---

## 7. 置信度声明（按推理专家协议）

| 结论 | 置信度 | 核心假设 |
|------|--------|----------|
| 提示词缓存是最大 token 杠杆、我方缺断点与指标 | **高** | 代码级事实（anthropic.rs 无 cache_control；CostLedger 已记账未派生指标） |
| CC/Codex 内部机制描述（缓存分层/fork 压缩/加密摘要/defer_loading） | **中** | 公开博文与第三方分析，无源码级证据；具体数值随版本漂移 |
| P0.1 断点注入对 DeepSeek 端点有效 | **中** | DeepSeek anthropic 兼容端点识别 `cache_control`（需双请求实测验证） |
| 压缩升级（fork 摘要）净收益为正 | **中** | 额外 API 成本 < 主线上下文污染省下的 token（需基准验证） |
| 安全/质量闭环已领先 CC/Codex | **高** | 公开能力对照（CC 官方自述"规则不可靠，用 hook/权限"） |

**建议的落地顺序**：P0.1→P0.2→P0.8（先打通缓存链路与可观测），随后 P0.3–P0.7 批量处理
代码级缺陷，P1 按 1.1→1.4→1.6 推进。每档合入前先出设计评审记录（沿用
`docs/superpowers/plans/` 惯例），并在 AGENTS.md §5 错误档案登记新发现的缺陷模式。

---

## 附录 A：本报告引用的关键代码位置速查

| 主题 | 位置 |
|------|------|
| DEFAULT_SYSTEM_PROMPT | `agent/src/prompts.rs:13-75` |
| 主循环 | `agent/src/loop_impl.rs:151-1074`（单轮 1133-1970） |
| 压缩链 | `agent/src/memory.rs:162-303`（L1/L2）；`agent/src/compaction.rs:52-99`（L3） |
| 预算控制器 | `agent/src/budget/controller.rs:23-29`（128K/32K） |
| 前缀构建 | `context/src/lib.rs:304-449`（CacheAwarePromptBuilder） |
| 原子压缩 | `context/src/history.rs:313-404`（HistoryCompactor/AtomicUnitCompactor） |
| Anthropic 请求 | `provider/src/anthropic.rs:112-156, 661-671, 851-857` |
| OpenAI 请求 | `provider/src/openai.rs:123-173, 542-560` |
| 工具 schema 缓存 | `provider/src/tool_cache.rs:42-80` |
| 成本记账 | `provider/src/cost.rs:115-207`（含 cache hit/miss 记账） |
| 子代理预设 | `agent/src/delegate.rs:100-169`；`sub_agent.rs:38, 562-570, 708` |
| MCP 适配 | `mcp/src/adapter.rs` |
| 评分卡 | `metrics/src/lib.rs:196-423`（四维 + protocol + composite） |
| 只读分类器 | `security/src/readonly.rs`（四层 + Dangerous） |
| 路径守卫 | `security/src/path.rs:23-90`（secure_resolve 双检查） |
| serve 路由 | `serve/src/lib.rs:453-478` |

> 全文完（2026-08-12）。下一步：按 §5 P0 顺序开票，先做 P0.1 的前置实测实验。

