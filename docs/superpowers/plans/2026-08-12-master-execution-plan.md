# DeepseekNova 全面执行计划书 — 十大领域（工作流 / 上下文 / 工具 / MCP / 安全 / 成本 / 自动化 / 提示词无关能力 / 多 Agent 并行）

> 日期：2026-08-12　｜　类型：架构级全面执行计划（Master Execution Plan）　｜　状态：待多轮审核
> 前置调研：`docs/superpowers/plans/2026-08-12-benchmark-claude-code-codex.md`（差距分析 + P0/P1/P2 路线图，P0 已完成并合入）。
> 本文档是**执行计划书**：先经至少两轮审核（领域覆盖 + 技术正确性）通过后开始实现；全部实现完成后复检 → 修复 → 再复检 → 提交。

---

## 0. 执行模式（按 AGENTS.md §1.1 完整推理专家协议）

| 阶段 | 要求 | 本文档落实 |
|------|------|-----------|
| 错误预扫描 | 进入问题前先设立禁行区 | §0.1 禁行区清单 |
| 思考透明化 | 完整推理链路，不跳跃 | 每领域：现状证据 → 差距 → 目标 → 多路径 → 验证 → 反例 |
| 自我质疑 | 主动寻找反例、失效边界 | 每项含「反例/失效边界」 |
| 多路径探索 | 至少两条本质区别的路径 | 关键决策均给出路径 A/B 对比 |
| 复现拦截 | 输出前逐条审计 | §8 验收清单逐条核对 |
| 置信度声明 | 每次结论标注置信度 | §9 |

### 0.1 禁行区（本次执行绝不触碰）

1. **不破坏 DeepSeek replay 不变量**：assistant(tool_calls) → Tool 结果的原子配对、load-bearing reasoning 不得丢（`ValidatedRequest` 强制）。
2. **不削弱安全边界**：readonly 分类器、路径双守卫、hard_deny、沙箱档位、QualityPolicy 确定性规则一律只强化不放松。
3. **不改变零配置路径的默认行为**：新能力默认关闭或默认安全，显式开启才生效（回归防线，参照 `[protocol] enabled=false` 惯例）。
4. **不改 provider 请求的向后兼容**：未显式配置时请求体与旧版本逐字节一致（P0.1 已验证的 `cache_control=false` 路径同理）。
5. **不新增第三方重依赖**：全部用现有依赖（tokio/serde/serde_json/tree-sitter/rusqlite/reqwest），避免引入供应链风险。
6. **不删除测试**：新增能力必须附带测试；既有测试只增不改语义（修 bug 例外，须留无桩回归测试）。

---

## 1. 十大领域 → 实施块映射

| # | 用户需求 | 对应领域 | 实施块 | 计划章节 |
|---|----------|----------|--------|----------|
| 1 | AI 工作流 | 阶段化执行（Understand/Plan/Execute/Verify/Reflect/Distill）+ 质量闭环 | A | §4.1 |
| 2 | 上下文工程 | 三层缓存分层 / 压缩链升级 / 注入预算 / 前缀稳定性 | B | §4.2 |
| 3 | 工具调用 | 工具 schema 缓存 / 并发调度 / 只读缓存 / 结果截断与回填 | A+C | §4.3 |
| 4 | 函数调用 | 结构化输出契约 / JSON 提取校验重试 / reasoning 回放 | A | §4.4 |
| 5 | MCP 模型上下文协议 | defer_loading 桩 / 工具发现 / 适配强化 / 安全边界 | C | §4.5 |
| 6 | 安全护栏 | 能力裁剪 / 策略对齐 / 对抗审查 / 沙箱边界 / 注入防御 | E | §4.6 |
| 7 | 成本优先 | 缓存命中率门禁 / effort 路由 / 压缩降本 / eval 成本度量 | F | §4.7 |
| 8 | 自动化 | 自动验证闭环（LSP/确定性 verify）/ eval 基准 / CI 门禁 / serve 持久化 | A+F | §4.8 |
| 9 | 提示词无关能力 | harness 确定性承担任务质量：门控/契约/验证不依赖提示词 | A（主线） | §4.9 |
| 10 | 多 Agent 并行 | 子代理文件化 / 嵌套深度 / 并行编排 / 结果聚合 | D | §4.10 |

---

## 2. 现状盘点（代码级证据，源自前序调研）

| 领域 | 现状 | 位置 |
|------|------|------|
| 工作流 | Observe→Plan→Tool→Verify→Reflect→Distill 六阶段 + PhaseRunner 门控 + 失败回炉 + 评分卡 | `agent/agent/loop_impl.rs` / `agent/phase_runner.rs` |
| 上下文 | L1 截断 + L2 滑动窗口 + L3 LLM 摘要；`CacheAwarePromptBuilder` 前缀 hash 诊断；recall 注入无预算 | `agent/memory.rs` / `agent/compaction.rs` / `context/lib.rs` |
| 工具 | 16 内置工具 schema ≤6700 字符；读并发写串行；只读结果缓存；ToolSchemaCache 按地址缓存 | `tools/src/lib.rs` / `provider/tool_cache.rs` |
| 函数调用 | 流式 tool call 累积 + replay 不变量强制 + Anthropic/OpenAI 双协议 | `provider/anthropic.rs` / `provider/openai.rs` |
| MCP | stdio/HTTP 客户端 + 发现 + 适配；工具全量注入（无桩） | `mcp/` |
| 安全 | 权限门 allow/ask/deny/hard_deny + 只读四层分类 + 路径双守卫 + 三平台沙箱 + QualityPolicy | `permission/` / `security/` / `sandbox/` |
| 成本 | CostLedger 记账 + cache_hit_rate 指标（P0.8 已落地）+ cache_control 断点（P0.1 已落地） | `provider/cost.rs` / `anthropic.rs` |
| 自动化 | LSP 编辑后诊断 + 确定性 verify + eval 命令 + serve runs/sessions 持久化 | `tools/lsp.rs` / `verify.rs` / `cli/eval.rs` / `serve/` |
| 提示词 | `DEFAULT_SYSTEM_PROMPT` ≈1.1–1.3K token；子代理 4 内置预设硬编码；replay 由 ValidatedRequest 强制 | `agent/prompts.rs` / `delegate.rs` |
| 多 Agent | `SubAgentRunner` + `DelegateEngine` 4 预设，并发 2，深度 3；Coordinator 图执行 parallel 节点 | `agent/sub_agent.rs` / `delegate.rs` / `coordinator.rs` |

---

## 3. 总体实施顺序（依赖关系）

```
A（提示词无关 harness + 工作流/函数调用/自动化验证）
  └→ B（上下文工程：依赖 A 的验证闭环判断压缩质量）
  └→ C（工具/MCP：依赖 A 的工具调度契约）
  ├→ D（多 Agent：依赖 A 的角色契约 + C 的工具契约）
  ├→ E（安全护栏：贯穿 A–D，最后统一强化收口）
  └→ F（成本/eval/CI：依赖 A–E 稳定后建设基准，防止基准被实现噪音污染）
```

> 每个实施块完成后：`cargo fmt` + 聚焦测试 + 反例回扫；跨 crate 变更跑 `make check`。
> 全部完成后：§8 复检清单逐项验收 → 修复 → 再复检 → 提交。

---

## 4. 十大领域详细设计

> 每项格式：现状证据 / 差距 / 目标 / 多路径探索（A/B）/ 改动位置 / 验证 / 反例与失效边界。
> 跨 crate 改动按 AGENTS.md §1.1 走完整协议。

### 4.1 AI 工作流（阶段化执行 + 质量闭环）

**现状**：六阶段循环 + `PhaseRunner` 门控（Understand 首轮 / Plan 每轮）+ 失败回炉 `reflect_retry` + 四维评分卡 + 失败诊断。质量钩子链（ToolHook before/after）已装配。

**差距**：
1. 阶段推进依赖提示词中的阶段角色文本（`render.rs` 的 `[verification failed]` / `[Pre-completion review]` 回炉文案），模型偏离阶段时靠门控检测——门控是确定性的（好），但**回炉路径**依赖模型自我修正，缺少"确定性恢复"兜底。
2. Plan 阶段在双模型 Coordinator 中完整，但单 agent 路径的 Plan 是轻量门控，无结构化计划产物。

**目标**：工作流由 harness 确定性驱动——阶段推进、违规检测、失败回炉全部有确定性与模型两条腿；阶段产物（计划/验证证据）结构化落盘。

**多路径探索**：
- **路径 A（确定性恢复优先）**：失败回炉不再仅依赖 `reflect_retry`（模型反思），先执行确定性恢复（自动重跑失败工具 + 自动裁剪工具参数 + LSP 诊断重跑），模型反思作为第二层。收益：弱模型也能恢复；风险：确定性恢复可能掩盖模型错误决策——用"恢复次数上限 + 恢复后必须重新验证"护栏。
- **路径 B（保持现状 + 增强观测）**：维持模型主导回炉，仅增强 diagnose 记录回炉决策链。收益：零行为变化；上限低。
- **推荐**：A 的"先确定性后模型"顺序，恢复次数上限 2。

**改动位置**：`agent/agent/loop_impl.rs` 完成路径 / `agent/verify.rs` / `agent/reflection.rs`。
**验证**：确定性恢复单元测试（模拟工具失败 → 自动重试成功）；回归测试确认模型反思路径仍可触发。
**反例**：确定性重试对"参数本身错误"无效——重试条件必须限定为"工具可重试错误类别"（retryable 错误 / 超时 / 输出截断），参数错误不重试直接回炉模型。

### 4.2 上下文工程

**现状**：L1 截断（`shrink_large_results`）+ L2 滑动窗口（`slide_window` 保留 System 前缀）+ L3 LLM 摘要（`compaction.rs`，`MAX_STRIKES=3` 熔断）；`CacheAwarePromptBuilder` 前缀 hash；recall 注入无预算；`AtomicUnitCompactor` 有 Tool+reasoning 形态可疑点（P1.1 B 步）。

**差距**：
1. L3 摘要跑在主线上下文（无 fork 独立调用）；压缩前不阻塞写工具；摘要后不保留"最近消息 + 摘要"结构（Codex 模式）。
2. ~~三层缓存分层（global/project/session）未实现~~ **已由 B6 落地**（2026-08-31 复核：`CacheAwarePromptBuilder::build_prefix` 段序 system → AGENTS.md（global）→ DEEPSEEKNOVA.md（project）→ repo map，专项测试 `cache_aware_prefix_layers_agents_then_project` 覆盖；session 层=对话内动态注入本就后置，无需独立机制）——原差距描述过期。
3. recall 注入无 token 预算；压缩后不重注入 invoked skills（CC 模式）。

**目标**：压缩质量不依赖模型提示词（摘要提示词内聚为独立契约）；前缀三层稳定；注入全部预算化。

**多路径探索**：
- **路径 A（fork 摘要，对齐 Codex）**：压缩时用独立 Provider 调用（同一 system 前缀 + 历史摘要请求），产出"最近消息 + 摘要"结构；写工具压缩前阻塞。收益：主线上下文不被摘要污染；风险：多一次 API 调用成本——用 `[budget]` 控制触发，仅当 L1/L2 无法满足时触发。
- **路径 B（主线摘要 + 修复形态）**：保留主线 L3，修 `AtomicUnitCompactor`（摘要改 User 角色、不克隆 reasoning）、写前阻塞、保留最近 N 条。收益：改动小；上限低于 A。
- **推荐**：先 B 后 A（B 立即可做，A 作为 B 的迭代增强）。

**改动位置**：`context/history.rs` / `agent/memory.rs` / `agent/compaction.rs` / `agent/loop_impl.rs` / `context/lib.rs`。
**验证**：压缩后 replay 不变量通过；摘要质量对比（关键决策链保留断言）；fork 前后 input token 对比。
**反例**：fork 摘要必须把此前摘要链纳入输入（防信息累积丢失）；三层分层必须保持段序（静态优先动态最后），否则缓存全 miss。

### 4.3 工具调用

**现状**：16 内置工具 schema ≤6700 字符预算（`tools/src/lib.rs` 有 `MAX_SCHEMA_CHARS` 测试兜底）；读工具并发（JoinSet）、写工具串行；只读工具结果会话缓存（写后整体失效）；`ToolSchemaCache` 按工具地址集合缓存序列化 payload；P0.6 已把 `tool_map` 提出循环。

**差距**：
1. 工具描述以自然语言为主——对弱模型，机器可读的 JSON schema + 参数约束比长描述更可靠（提示词无关能力的核心：**工具契约承载质量**）。
2. 工具结果截断是字节级头尾截断（`shrink_large_results`），无"结构化截断"（保留关键字段）。

**目标**：工具 schema 精确（参数 required/enum/pattern 约束齐全）、结果截断保留关键信息、调度策略可配置。

**改动位置**：`tools/src/*.rs`（schema 参数约束补齐）/ `agent/memory.rs`（截断增强）。
**验证**：schema 校验测试（缺 required 参数报错）；截断保留关键字段测试。
**反例**：schema 收紧可能破坏既有调用形态——补参数前 grep 所有调用点（AGENTS.md §5 已归档"批量替换误伤上下文"教训）。

### 4.4 函数调用（结构化输出契约）

**现状**：流式 tool call 累积（Anthropic tool_use / OpenAI tool_calls）+ `ValidatedRequest` replay 不变量强制 + reasoning signature 回放（缺 signature → 400）+ `review::extract_json` 宽松 JSON 提取。

**差距**：
1. 结构化输出（JSON）解析是"宽松提取 + 失败回退文本"（`parse_plan` / reflect / verify 判定均如此）——对弱模型，需要**契约化**：schema 声明 + 校验失败自动重试（带错误回显）。
2. 无统一的"结构化输出契约"模块（JSON Schema 校验、错误回显重试、token 预算内的重试上限）。

**目标**：新增 `agent/contract.rs`：结构化输出契约（`extract → validate → 失败回显重试（≤2 次）→ 回退默认`），供 plan/reflect/verify/review 全部 LLM 判定复用。

**多路径探索**：
- **路径 A（集中契约模块）**：新建 `contract.rs`，统一 extract/validate/retry；各判定点迁移。收益：一次实现处处受益，弱模型任务质量显著提升；风险：迁移面广——按调用点逐个迁移并保留回退。
- **路径 B（仅补校验重试）**：各判定点单独补"校验失败回显重试"。收益：改动局部；风险：逻辑重复，维护成本高。
- **推荐**：A。

**改动位置**：新建 `agent/src/contract.rs`；迁移 `coordinator.rs::parse_plan` / `reflect` / `verify` / `review` 的 JSON 判定。
**验证**：契约模块单测（合法/非法/截断 JSON、重试上限、回退默认）；各判定点迁移后原测试保持绿。
**反例**：重试上限内模型反复输出非法 JSON → 必须回退默认（不无限重试烧 token）；错误回显必须脱敏（防注入）。

### 4.5 MCP 模型上下文协议

**现状**：`mcp/` crate：stdio/HTTP 客户端、发现（discovery）、适配（adapter.rs 把 MCP 工具映射为 `Tool`）、SSRF/域名策略复用。

**差距**：
1. MCP 工具全量注入前缀（无 `defer_loading` 桩）——大量 MCP 工具会膨胀固定前缀并破坏缓存稳定性（CC 用桩 + 选中才拉全 schema）。
2. 工具发现是静态的，无"按需加载"能力位。

**多路径探索**：
- **路径 A（defer_loading 桩）**：适配层先注入桩（name + `defer_loading:true`），模型选中时才展开全 schema。需实测 DeepSeek 端点是否支持（P0.1 同款实测流程）；不支持则退化为"工具集快照稳定 + 按需裁剪"。
- **路径 B（会话级工具集裁剪）**：启动时按配置裁剪 MCP 工具集，保持前缀稳定。收益：无需端点支持；风险：静态裁剪不灵活。
- **推荐**：先 B（立即可做、零端点依赖）保前缀稳定，A 作为实测通过后的增强。

**改动位置**：`mcp/adapter.rs` / `mcp/discovery.rs` / `provider/openai.rs`（桩字段）。
**验证**：裁剪后前缀 hash 稳定测试；桩字段序列化测试。
**反例**：桩被模型调用但展开失败 → fail-closed 报错，不静默。

### 4.6 安全护栏

**现状**：权限门（allow/ask/deny/hard_deny + 会话缓存 + 速率限制 + 审计）；readonly 四层分类 + 危险形态硬拒；路径双守卫（词法归一 + symlink canonicalize）；三平台沙箱三档；QualityPolicy 确定性规则（0 token）；ToolHook 链 fail-closed；`sanitize_output` 中和权限修改指令形状；子代理输出净化；对抗审查（协议默认关）。

**差距**：
1. 子代理文件化后（§4.10）agent 文件是新的提示词注入面——frontmatter 校验 + 能力裁剪必须同步（P1.4 已规划）。
2. 对抗审查默认关——面向"弱模型完美做任务"，对抗审查是 harness 侧质量护栏，需评估默认开启的 token 成本（仅对写操作开启，可配）。

**目标**：新注入面全部安全闭环；对抗审查对写操作默认开（`[protocol] adversarial_review` 语义化调整），成本受控。

**改动位置**：`agent/delegate.rs`（agent 文件加载校验）/ `agent/mod.rs`（对抗审查触发条件）/ `security/`（无削弱）。
**验证**：agent 文件注入测试（恶意 frontmatter 被拒）；对抗审查写操作触发 + 成本测试。
**反例**：默认开启必须不影响零配置路径（`[protocol] enabled=false` 时保持关闭）。

### 4.7 成本优先

**现状**：P0.1 `cache_control` 断点（默认开可关）+ P0.8 `cache_hit_rate` 指标（CLI 摘要 + <60% 提示）+ CostLedger 记账 + effort 路由（quick/high）+ auto 路由（整轮）。

**差距**：
1. 无缓存命中率**门禁**（eval/CI 中 `cache_hit_rate` 低于阈值判失败）。
2. eval 无成本/轮数/命中率字段的基准报告（P1.6 已规划）。
3. `AtomicUnitCompactor` 等压缩路径的成本未度量（压缩精度 vs 成本）。

**目标**：成本成为一等验收维度：eval 报告含 token/命中率/轮数；CI 门禁；压缩触发可观测。

**改动位置**：`cli/src/eval.rs`（扩展字段）/ `evals/` 新目录 + `evals/results/` / `metrics/`（压缩事件记录）。
**验证**：eval 报告含新字段测试；门禁阈值测试。
**反例**：命中率门禁在 mock/无缓存端点会误杀——门禁仅对配置了缓存计费的端点生效（`cache_hit_rate` 为 None 时跳过）。

### 4.8 自动化

**现状**：LSP 编辑后诊断（write/edit/move 后自动注入）+ 确定性 verify（bash 经 SecurityContext）+ LLM verify（可选）+ eval 命令 + serve runs/sessions 持久化 + `/v1/runs/{id}/resume`。

**差距**：
1. 自动化验证闭环无"验证命令发现"能力（CC 从工具结果/命令历史推断验证命令；我们依赖配置 `verify.commands`）。
2. eval 基准任务集不存在（无 `evals/` 目录）。

**目标**：`evals/` 基准（20–50 例覆盖单文件/多文件/跨语言/调试/长会话/压缩边界）+ CI 门禁（`eval --require-min-score` 已支持）；验证命令发现作为增强。

**改动位置**：`cli/src/eval.rs` / `evals/` + `evals/results/` / `verify.rs`（命令发现增强）。
**验证**：基准任务集可运行；CI 门禁脚本（`scripts/eval-ci.sh` 或 Makefile 目标）。
**反例**：eval 任务过拟合——按领域分层、留 20% 盲测（P1.6 已定）。

### 4.9 提示词无关任务能力（主线）

**核心命题**：让接入的模型**不需要提示词**也能完美做任务——任务质量由 harness 的**确定性机制**承担，提示词只是行为基线而非质量来源。

**现状**：`DEFAULT_SYSTEM_PROMPT` ≈1.1–1.3K token 承载执行契约；但**质量已部分由确定性机制承担**（replay 不变量强制、permission gate、ToolHook fail-closed、verify 确定性规则、LSP 诊断、QualityPolicy 0-token 规则）。这证明方向可行，但分布零散。

**差距**（提示词承载但应迁移到 harness 的部分）：
| 提示词职责 | 现状 | 应迁移为 |
|-----------|------|----------|
| "Read before writing" | 提示词 + gate 无强制 | **确定性前置**：写工具执行前强制要求"已读取相关文件"证据（ToolHook before 检查） |
| "验证变更" | 提示词 | **确定性验证闭环**：写后强制 LSP 诊断 + 确定性 verify（已部分有） |
| "不要编造事实" | 提示词 | **grounding 强制**：工具结果必须回填（replay 不变量已强制）；禁止无证据断言（review 层检测） |
| "保持最小改动" | 提示词 | **diff 审查**：写后 diff 统计（改动行数/文件数）注入结果，超阈值告警 |
| "失败要诊断" | 提示词 | **失败诊断**：`DiagnoseReport` 已自动生成（已 harness 化） |
| 结构化输出 | 提示词 | **契约模块**（§4.4）强制 |

**多路径探索**：
- **路径 A（全面 harness 化）**：把上表 5 项逐条迁移为确定性机制（ToolHook / 契约 / diff 统计 / grounding 审查），提示词逐步瘦身（只保留角色与通信规范）。收益：任何模型（含弱模型）任务质量稳定；风险：迁移面大、每项需测试；提示词瘦身可能让强模型行为"退化"（失去精细指令）——**瘦身前提是 harness 机制覆盖了原提示词全部关键职责，逐条验证后缩**。
- **路径 B（harness 增强 + 提示词不动）**：只加确定性机制，提示词保持现状。收益：零风险；风险：提示词继续承载质量，弱模型仍不稳。
- **推荐**：A，但**提示词瘦身必须在 harness 覆盖验证后分步进行**（先加机制，后缩提示词，每步跑 eval 对比）。

**改动位置**：
- 写前置读取强制：`agent/quality.rs`（ToolHook before）+ `tools/fs.rs`（写工具记录读取依赖）。
- diff 审查：写后 ToolHook after 统计 diff（复用 `checkpoint::diff_summary`）。
- grounding 审查：`agent/review.rs`（无证据断言检测）。
- 提示词瘦身：`agent/prompts.rs`（分步，先删"已 harness 化"职责，保留角色/身份/通信）。

**验证**：每步迁移配 eval 对比（迁移前 vs 后，通过率不下降 + 弱模型提升）；提示词 token 下降记录。
**反例**：过度 harness 化会锁死强模型灵活性（如"最小改动"强制可能阻止合理的大重构）——机制必须是**可配置的软约束**（默认开启，阈值可调），非硬拒绝。

### 4.10 多 Agent 并行

**现状**：`SubAgentRunner`（独立上下文、只回最终文本）+ `DelegateEngine` 4 内置预设（explorer/coder/tester/reviewer，代码硬编码）+ 并发 2 + 深度 3 + `sanitize_output` 净化；Coordinator `Action::Parallel` 图节点并发。

**差距**：
1. 角色硬编码，用户不可自定义（CC：`.claude/agents/*.md`）。
2. 深度 3 vs CC 5；无 hand-off 消息模式。
3. 并行编排中间结果不进父上下文（CC 的 script variables 模式）——我们有 coordinator history 上限（50 条/2000 字符）近似，但未结构化。

**多路径探索**：
- **路径 A（文件化，对齐 CC）**：`.deepseeknova/agents/*.md`（frontmatter: name/description/model/tools + body 系统提示词），复用 skills 解析器（`SkillResolver` 同源）；深度 3→5；hand-off 结构化交接文本。收益：用户可定义角色、对齐行业生态；风险：注入面（§4.6 已规划防护）。
- **路径 B（配置化演进）**：沿用 `TaskSpec` + `[delegate.agents]` 配置扩展预设。收益：无需文件格式；上限低。
- **推荐**：A。

**改动位置**：`agent/delegate.rs` / `agent/sub_agent.rs` / `agent/task_spec.rs` / `skills/`（解析器复用）/ `agent/mention.rs`。
**验证**：agent 文件加载/覆盖/校验测试；嵌套 5 层集成测试；hand-off 文本契约测试；并行编排结果聚合测试。
**反例**：agent 文件是提示词注入面——frontmatter 校验 + sanitize + 能力裁剪必须同步（§4.6）；嵌套深度提升必须防死循环（`DelegateDepth` 已存在，沿用）。

---

## 5. 实施块工作项拆分（A–F）

> 每个工作项：改动位置 / 验证方式 / 涉及 crate / 依赖。合并时按 AGENTS.md §5 错误档案登记新发现的缺陷模式。

### 块 A — 提示词无关任务能力 harness（§4.1/4.3/4.4/4.8/4.9）

| # | 工作项 | 改动位置 | 验证 | 依赖 |
|---|--------|----------|------|------|
| A1 | **结构化输出契约模块** `agent/src/contract.rs`：extract→validate→失败回显重试（≤2）→回退默认；JSON Schema 校验（`serde_json` + 手写约束） | 新建 `agent/src/contract.rs` | 单测：合法/非法/截断/重试上限/回退 | 无 |
| A2 | **迁移判定点**：`parse_plan` / `reflect` / `verify` / `review` 的 JSON 判定改走 `contract.rs` | `agent/coordinator.rs` / `reflect.rs` / `verify.rs` / `review.rs` | 迁移后各原测试保持绿 + 新增非法 JSON 回显重试测试 | A1 |
| A3 | **写前读取证据强制**（ToolHook before）：写工具（write/edit/move）执行前要求会话内已有对目标文件的读取记录，否则 Blocking 拒绝（可配置豁免） | `agent/quality.rs` / `tools/fs.rs` | 单测：未读直接写被拒；读过放行；**新建文件（无读取记录）豁免**；豁免开关测试 | 无 |
| A4 | **写后 diff 审查**（ToolHook after）：复用 `checkpoint::diff_summary`（`async fn`，ToolHook after 为 async 上下文可直接 await）统计改动行数/文件数，超阈值告警注入 ToolResult | `agent/quality.rs` / `checkpoint/` | 单测：diff 统计阈值触发 | 无 |
| A5 | **确定性恢复优先**：失败回炉先确定性恢复（重试 retryable 错误类别 ≤2 次），再模型反思 | `agent/loop_impl.rs` / `agent/reflection.rs` / `agent/verify.rs` | 单测：retryable 失败自动重试成功；参数错误不重试 | 无 |
| A6 | **提示词瘦身（分步）**：删除已 harness 化的职责段落（验证/最小改动/失败诊断），保留角色/身份/通信；每步跑 eval 对比 | `agent/prompts.rs` | 每步：eval 通过率不下降 + `DEFAULT_SYSTEM_PROMPT` token 下降 | A1–A5 |
| A7 | **grounding 审查**：review 层检测"无工具证据的断言"（内容含结论性语句但无对应工具结果） | `agent/review.rs` | 单测：无证据断言被标记 | A2 |
| A8 | **验证命令发现**（§4.8 差距②落地）：从最近工具结果/命令历史推断验证命令（`cargo check`/`tsc --noEmit` 等），注入 verify 提示；配置 `verify.commands` 仍优先 | `agent/verify.rs` / `agent/memory.rs` | 单测：工具结果含编译命令 → 推断为验证命令；显式配置优先 | A2 |

### 块 B — 上下文工程（§4.2）

| # | 工作项 | 改动位置 | 验证 | 依赖 |
|---|--------|----------|------|------|
| B1 | **修 `AtomicUnitCompactor` 形态**：摘要改 User 角色、不克隆 reasoning_content | `context/history.rs:326-403`（struct 326 / impl 328-403） | 压缩后 replay 不变量通过；原测试更新 | 无 |
| B2 | **写前阻塞**：压缩触发前阻塞写工具（对齐 Codex） | `agent/loop_impl.rs` | 单测：压缩进行中写工具被阻塞/排队 | 无 |
| B3 | **保留最近消息**：压缩后保留最近 N 条（默认 10 条/可配）+ 摘要 | `agent/memory.rs` / `agent/compaction.rs` | 单测：压缩后最近消息保留、摘要存在 | 无 |
| B4 | **recall 注入预算化**：按 `max_recall_tokens`（默认 2000）裁剪注入块 | `agent/tools.rs` / `agent/compaction.rs` | 单测：注入大小受限 | 无 |
| B5 | **fork 摘要（迭代增强）**：L3 改为独立 Provider 调用（同 system 前缀），摘要链纳入输入 | `agent/compaction.rs` | 聚焦测试 + eval 对比 input token | B1–B3 |
| B6 | **三层缓存分层（P2.1）**：global（AGENTS.md 等全局规则）/ project（DEEPSEEKNOVA.md）/ session 分层注入，段序静态优先 | `context/lib.rs` / `agent/mod.rs` | 前缀 hash：全局规则变化只 invalidate 全局段 | B4 |

### 块 C — 工具与函数调用 + MCP（§4.3/4.5）

| # | 工作项 | 改动位置 | 验证 | 依赖 |
|---|--------|----------|------|------|
| C1 | **MCP 工具集裁剪**：启动时按配置裁剪 MCP 工具，保持前缀稳定 | `mcp/adapter.rs` / `mcp/discovery.rs` / `config` | 前缀 hash 稳定测试 | 无 |
| C2 | **defer_loading 桩（实测后）**：桩注入 + 选中才拉全 schema；端点不支持则保持 C1 | `mcp/adapter.rs` / `provider/openai.rs` | 端点实测 + 桩序列化测试 | C1 |
| C3 | **工具 schema 参数约束补齐**：关键工具（fs/grep/shell/web_fetch）补 required/enum/pattern，不超 `MAX_SCHEMA_CHARS` 预算 | `tools/src/*.rs` | schema 预算测试仍绿 + 新约束测试 | 无 |

### 块 D — 多 Agent 并行（§4.10）

| # | 工作项 | 改动位置 | 验证 | 依赖 |
|---|--------|----------|------|------|
| D1 | **子代理文件化**：`.deepseeknova/agents/*.md`（frontmatter: name/description/model/tools + body），复用 skills 解析器 | `agent/delegate.rs` / `skills/` / `agent/sub_agent.rs` | 加载/覆盖/校验测试 | C3 |
| D2 | **嵌套深度 3→5** + 防死循环（沿用 `DelegateDepth`） | `agent/sub_agent.rs` / `agent/recursion.rs` | 嵌套 5 层集成测试 | D1 |
| D3 | **hand-off 消息模式**：子代理返回结构化交接文本（`## Handoff` 段） | `agent/delegate.rs` / `agent/sub_agent.rs` | hand-off 契约测试 | D1 |
| D4 | **并行编排结果聚合**：Coordinator `Action::Parallel` 结果结构化聚合（不全部进父上下文） | `agent/coordinator.rs` / `core/executor.rs` | 并行节点聚合测试 | D1 |

### 块 E — 安全护栏强化（§4.6）

| # | 工作项 | 改动位置 | 验证 | 依赖 |
|---|--------|----------|------|------|
| E1 | **agent 文件注入防护**：frontmatter 白名单校验 + sanitize + 能力裁剪 | `agent/delegate.rs` / `security/sanitize.rs` | 恶意 frontmatter 拒绝测试 | D1 |
| E2 | **对抗审查写操作默认化评估**：`[protocol] adversarial_review` 语义化（写操作默认开、只读不开），成本受控 | `agent/mod.rs` / `config` | 写操作触发 + 成本测试；`enabled=false` 保持关闭 | A4 |

### 块 F — 成本优先落地（§4.7/4.8）

| # | 工作项 | 改动位置 | 验证 | 依赖 |
|---|--------|----------|------|------|
| F1 | **eval 字段扩展**：报告含 total token / cache_hit_rate / rounds / 综合分 | `cli/src/eval.rs` / `metrics/` | 报告字段测试 | 无 |
| F2 | **eval 基准任务集**：`evals/` 20–50 例（分层 + 20% 盲测） | `evals/` + `evals/results/` | 基准可运行；`make eval-ci` 目标 | F1 |
| F3 | **CI 门禁**：`make eval-ci`（eval --require-min-score + 命中率阈值，仅对缓存端点生效） | `Makefile` / `scripts/` | CI 脚本 dry-run | F2 |
| F4 | **压缩事件观测**：diagnose 记录压缩次数/阈值/摘要长度 | `agent/diagnose.rs` / `metrics/` | 诊断报告含压缩事件测试 | B1 |

---

## 6. 验收标准（每个实施块）

1. `cargo fmt` + 聚焦测试通过（新增测试覆盖正向/反向/边界）。
2. 跨 crate 变更 `make check` 通过（fmt + clippy -D warnings + workspace 测试 + doc -D warnings）。
3. 反例回扫：计划书中该块标注的反例逐条验证（有测试或明确不适用理由）。
4. 默认行为不变：零配置路径（无新配置项）下请求体/行为与变更前一致。
5. 每块合入前在 AGENTS.md §5 登记新发现的缺陷模式（如有）。

---

## 7. 审核记录

| 轮次 | 日期 | 审核人 | 结论 | 修正内容 |
|------|------|--------|------|----------|
| 第一轮（领域覆盖 + 结构） | 2026-08-12 | 自审（AtomCode） | **通过（附 3 个待修项）** | ①标题"九大领域"与 10 项需求不符→改"十大领域"；②§5 工作项缺"验证命令发现"（§4.8 差距提到未落地）；③§8 复检清单缺"修复后重跑范围"闭环说明 |
| 第二轮（技术正确性 + 反例） | 2026-08-12 | 自审（AtomCode） | **通过（附 3 个待修项）** | ④B1 行号偏差：`history.rs:370-383` → 实际 `AtomicUnitCompactor` struct 在 `history.rs:326`、impl 328-403；⑤A3 缺"新建文件豁免"反例（一次性创建多个新文件时无读取记录，强制读取会拒绝合法新建）；⑥A4 备注 `diff_summary` 为 `async fn`（ToolHook after 为 async 上下文可调用，无阻塞问题） |
| 修正验收 | 2026-08-12 | 自审（AtomCode） | **通过** | ①–⑥ 全部应用：标题改"十大领域"（3 处）；§5 补 A8 验证命令发现；§8 加复检闭环规则 + A1–A8 范围；B1 行号修正；A3 补新建文件豁免；A4 补 async 备注。计划书两轮审核通过，可开始实施。 |

---

## 8. 复检清单（全部实施完成后逐项验收）

> **复检闭环规则**：任一复检项不通过时，修复该问题后**必须重跑该项及其全部依赖项**（修复可能引入连锁影响），再复检直至全部通过；每轮修复/复检结果更新 §7 审核记录。

- [ ] A1–A8 全部实现且测试通过（提示词无关 harness 完整）
- [ ] B1–B6 全部实现（上下文工程：压缩链 + 分层 + 预算）
- [ ] C1–C3 全部实现（工具/MCP 强化）
- [ ] D1–D4 全部实现（多 Agent 并行）
- [ ] E1–E2 全部实现（安全护栏强化）
- [ ] F1–F4 全部实现（成本/eval/CI）
- [ ] 反例回扫清单逐条核对（§4 各反例有测试或明确理由）
- [ ] `make check` 全绿（fmt / clippy / 测试 / doc）
- [ ] 零配置路径行为不变（回归确认）
- [ ] eval 基准对比：通过率不下降、token 下降、命中率提升（有数据）
- [ ] 计划书 §7 审核记录两轮均通过
- [ ] 提交（git commit）

---

## 9. 置信度声明

| 结论 | 置信度 | 核心假设 |
|------|--------|----------|
| 提示词无关能力方向可行（质量已部分由 harness 承担） | **高** | 代码级事实：replay 不变量/权限门/QualityPolicy/LSP 诊断已是确定性机制 |
| 结构化输出契约（A1/A2）显著提升弱模型任务质量 | **中-高** | 参照 CC/Codex 实践 + 本项目已有宽松提取路径；需 eval 量化 |
| fork 摘要（B5）净收益为正 | **中** | 额外 API 成本 < 主线上下文污染省下的 token；需基准验证 |
| MCP defer_loading（C2）端点支持 | **中** | DeepSeek 兼容端点是否识别 `defer_loading` 需实测（P0.1 同款流程） |
| 对抗审查默认开（E2）成本可控 | **中** | 仅写操作触发 + 输入/输出 cap（现有 6000/4000 字符）；需实测 |
| 子代理文件化（D1）对齐行业生态收益 | **中-高** | CC 已验证该模式；复用 skills 解析器降低工作量 |
| 提示词瘦身（A6）不降强模型质量 | **中** | 前提：harness 覆盖验证后分步缩，每步 eval 对比；反例已识别 |

---

## 附录 A：与既有文档的关系

- `2026-08-12-benchmark-claude-code-codex.md`：差距分析 + P0/P1/P2 路线图（P0 已完成合入）。本计划书是其 P1/P2 的**可执行化展开**，并把"提示词无关能力"提升为独立主线（§4.9）。
- 本计划书审核通过后，实施按 §3 顺序推进；每块完成更新 §7 审核记录与 §8 复检清单。

> 全文完（2026-08-12）。待 §7 两轮审核通过后开始实施。
