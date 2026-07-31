# DeepseekNova 循环完善与低成本优化计划

> 唯一设计原则：把 DeepSeek-V4-Flash 当作一个"低成本、高频决策引擎"，而不是一个
> "一次性回答机器"。核心循环：**Observe → Plan → Tool → Verify → Reflect → Next Action**，
> 配合长上下文、动态上下文检索与工具编排。

判定标准：循环的每个阶段都必须有**明确的代码路径**与**可观测事件**；默认路径不得绕过
关键相位。

### 0. 执行状态

| 阶段 | 状态 | 落地位置 |
|---|---|---|
| P1 并行工具 + Verify 回喂 | ✅ 已实现（PR #44，待合并） | `agent.rs` 分段调度、`verify.rs`、`[verify]` 配置 |
| P2 每步 effort / 观察压缩 / 工具缓存 | ✅ 已实现（含测试） | `agent.rs`（`with_effort_routing` / `with_observe_compression` / `with_tool_cache`）、CLI `step_effort_providers` |
| P3.1 真实 token 计量 | ✅ 已实现（含测试） | `core/tokens.rs`（官方口径 0.3/0.6 转换）、`agent/tokens.rs` 接入各截断点 |
| P3.2 中途检索 | ✅ 已实现 | 续聊含工具轮时召回 + 压缩驱逐后按最近用户意图召回（`MidRunRetrieval`，runtime 经记忆召回提供器装配） |
| P3.3 记忆/图检索增强 + 文件关联 | ✅ 已实现（含测试） | memory FTS5 trigram + LIKE 回退 + 嵌入检索接口（`search_hybrid`/`EmbeddingProvider`）；graph 符号 trigram 表；`record_task` 任务-文件关联 |
| P4.1 Coordinator 图工具 | ✅ 已实现 | `CoordinatorRunner::with_extension` 注入 GraphHandle、CLI 注册只读图工具 |
| P4.2 桌面端验证事件 | ✅ 已实现（前端本地无 Node，CI 复核） | `RunEvent::Verification` → `WireEvent::Verification` → TracePanel ✓/✗ 展示 |
| P4.3 默认 flash/pro 模板 | ✅ 已实现 | `setup.rs` 注释模板（角色分工 + P2/verify 示例）+ GUIDE.md |

> 注：嵌入后端默认仍为 `none`（不内置模型），`EmbeddingProvider` 接口与
> `search_hybrid` 混合检索已就绪，接入方实现 trait 即可启用。

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

---

## 7. P2 详细设计（高频决策经济学，本批次已实现）

### 7.1 每步 reasoning effort 动态路由

**现状**：effort 在创建 provider 时一次性定死；机械续步（列目录、读文件）与关键
决策（改设计、写代码）花同样的 reasoning tokens。

**设计**：
1. `EffortRouting { quick, high }`：双 provider 指针。`quick` 用 reasoning disabled，
   `high` 用高 effort。
2. 分类规则（零成本、无 LLM 往返）：上一条消息是 **Tool 结果且不含 `Error:`** →
   机械续步 → quick；首步 / 出错 / 回炉反馈 / 用户消息 → high。
3. `Agent::with_effort_routing(quick, high)`；CLI `step_effort_providers()` 按
   `config.agent.step_effort_routing` 构建；runtime 装配时 quick/high 缺失只告警并
   回退固定主 provider，不阻断构建。
4. 成本账本按实际 provider 自动计量（cost ledger 已按 provider 分桶）。

**反例与边界**：
- 工具结果无 Error 但模型需要深度推理 → 分类器判为 quick，省 token 但可能损失质量；
  兜底：任何 Error 都会回 high，且用户新消息永远走 high。
- 用 LLM 分类器替代规则 → 每次多一次往返，违背"低成本"原则，否决（本批次）。

### 7.2 工具结果观察压缩

**现状**：工具结果原始字符串 + 字符截断直接入历史。

**设计**：
1. `ObserveSettings { provider, threshold_chars, max_chars }`。
2. 触发：单条工具结果超过 `threshold_chars` 时，用廉价 provider（compact 指针，回退
   main）生成结构化摘要，要求保留路径/退出码/数字等关键事实。
3. 失败回退：压缩调用任何失败（provider 错误、空输出）→ 保持原始截断，不阻塞循环。
4. 缓存结果（`[cached]` 前缀）跳过压缩——已是最优形态。
5. 原始完整结果仍通过 `RunEvent::ToolResult` 发到 UI（事件不丢，历史才丢）。

### 7.3 会话内工具结果缓存

**设计**：
1. 只读工具按 `(name, arguments)` 的 SHA-256 前缀 64 位为 key 缓存结果。
2. 命中 → 结果前加 `[cached]` 注入，跳过执行；错误结果不缓存。
3. 失效：任何写类工具（write_file/edit_file/move_file/bash 等 `read_only()==false`）
   执行后清空整个缓存——读结果可能已过期，宁可保守。
4. `config.agent.tool_cache`（默认关，数据驱动后开）。

**P2 验收**：三特性均有配置开关、runtime 装配、CLI 接线与 agent 级测试
（effort 路由 quick/high 两态、压缩成功/失败回退、缓存命中/失效）。

---

## 8. P3 详细设计（上下文与检索工程）

### 8.1 真实 token 计量（CJK 感知）

**现状**：`estimate_tokens` = 字节数/4。中文 UTF-8 每字 3 字节 → 0.75 token/字，
而 DeepSeek 官方口径约 0.6 token/字；纯英文 4 字符 ≈ 1.2 token。旧算法对中文
会话系统性低估，压缩阈值失真。

**设计**（采用 DeepSeek 官方折算口径：
<https://api-docs.deepseek.com/quick_start/token_usage/>）：
1. `core::tokens`：`estimate_text_tokens`（英文 1 字符 ≈ 0.3 token，中文 1 字 ≈ 0.6
   token，整数运算 `(6*cjk + 3*other + 9)/10`）、`has_cjk`、`estimate_tokens(messages)`
   （含 reasoning_content）。
2. 预算换算：`char_budget_for_tokens(text, cap)` 按文本自身 CJK/ASCII 构成折算截断
   预算，替代旧的 `tokens*4` 固定换算。
3. `Memory::shrink_large_results` 改收 token 预算，内部换算，中文长结果不再被
   "4 倍放大"误伤。
4. provider 返回真实 usage 时以其为准（现状已有），本模块只用于压缩/注入的前置预算。

**反例与边界**：官方口径是近似值，不同模型偏差 ±20%；方向性选择——低估会让压缩
稍晚触发，但在 1M 上下文内风险可控；不做真实 BPE 的原因是零依赖 + 可单测，真实
tokenizer 留作未来可插拔项。

### 8.2 中途自动检索

**现状**：记忆召回只在会话起点；L3 压缩后重建已接入，但无去重、无周期触发。

**设计**：
1. 触发点三处：
   - 每轮新用户消息（已有）；
   - L3 压缩后按最近用户意图重建（已有）；
   - 周期触发：`config.memory.recall_every_steps`（默认 0 = 关），每 N 步按最近用户
     消息召回一次，覆盖"子目标切换"场景（子目标切换通常伴随多步工具轮次）。
2. 去重：`inject_recall` 对注入块做 SHA-256 记集合，相同块不重复注入（避免
   "每 5 步塞同一批记忆"）。
3. 注入形态不变：volatile User 消息 `<recalled-memory>`，不触碰 system 前缀，
   保住 DeepSeek 前缀缓存。
4. 召回块上限沿用 `recall_inject_tokens` 预算。

### 8.3 中文全文检索增强

**现状**：memory 与 graph 的 FTS5 均 `tokenize='porter unicode61'`；unicode61 把
中文按单字切分（无词义），porter 对 CJK 无效。

**设计**：
1. 两个 store 各增 trigram FTS 表（SQLite bundled ≥ 3.34 支持）；打开时若旧表已有
   数据而新表为空则回填。
2. 查询路由：查询含 CJK 且长度 ≥3 → trigram MATCH（引号转义同现有风格）；
   长度 1-2 → `LIKE '%q%'` 回退（trigram 至少需 3 字符）；纯拉丁 → 原 unicode61
   路径完全不变。
3. 增删同步：与主表同事务的 delete-then-insert 模式。

### 8.4 嵌入检索（P2 规划落地）

**现状**：`memory_meta` 已有 `embedding BLOB / embed_dim / embed_model` 列，
`config.memory.embedder`（none|local|remote）已定义但无消费方。

**设计**：
1. `EmbeddingProvider` trait（core）：`embed(&str) -> Result<Vec<f32>>`、`dim()`。
2. `LocalEmbedder`：确定性字符 bigram 词袋哈希向量（归一化），中英文通用、零依赖、
   可单测；作为 `embedder="local"` 的默认实现。
3. `MemoryStore::search_hybrid(query, limit, provider, model)`：FTS 候选 ∪ 嵌入余弦
   候选，`0.5*bm25归一化 + 0.5*cosine` 融合排序（内部可调）；无 provider / 无嵌入时
   行为与 `search()` 完全一致。
4. `engine.recall_hybrid` 包装；`embedder="remote"` 预留（需 OpenAI 兼容 embeddings
   端点，本批次只留接口与文档）。

### 8.5 记忆库与代码图索引打通

**设计**：runtime 召回闭包同时查询 MemoryEngine 与 GraphIndex，合并为单块：
`## Recalled Context`（记忆，优先保留）+ `## Related Symbols`（图实体，路径/签名），
总长度受 `recall_inject_tokens` 预算约束。合并逻辑为纯函数 `build_unified_block`
便于单测。两个 SQLite 库仍各自独立，不打乱 crate 依赖（core 不依赖 graph）。

---

## 9. P4 详细设计（产品化闭环）

### 9.1 Coordinator 补上图检索工具

**现状**：CLI coordinator 模式此前无条件排除图工具（"待 coordinator graph wiring
落地"的过时注释）；`CoordinatorRunner` 的 `with_extension` 机制已存在。

**设计**：`config.graph.enabled` 时注入 `GraphHandle` 扩展并在执行器注册
`search_code / traverse_graph / retrieve_entity` 为只读工具；规划器只暴露只读工具
（既有 `validate_plan_tool_boundary` 安全边界不变）；关闭时仍排除、工具降级提示不变。
同步清理过时注释。

### 9.2 桌面端验证事件视图

**现状**：P1 已把 `RunEvent::Verification` 贯通到 core `WireEvent` 与 serve/SSE；
桌面前端无类型、无处理、无 UI。

**设计**：frontend `WireEvent` 增 `verification` 变体；`EventHandlers` 增可选
`onVerification`；store 记录验证事件列表；Transcript 内渲染验证行（通过 = 绿色 ✓
命令名，失败 = 红色 ✗ + 截断摘要），无事件时不渲染。事件可选，旧调用方不受影响。

### 9.3 默认 flash/pro 角色分工模板

**现状**：模型角色路由（Task/Compact/Quick + review 指针）已存在，但无统一配置模板；
DeepSeek-V4-Pro 未上线。

**设计**：
1. 新配置节 `[model_pointers]`：`main / task / review / compact / quick`（全空 =
   不覆盖）。
2. 回退链：显式指针 → legacy `review.review_model` / `agent.compact_model` → 主模型。
3. `setup` 生成模板 + GUIDE 文档：当前默认 main/compact/quick = `deepseek-v4-flash`；
   `review` 预留 `deepseek-v4-pro`（注释说明上线后启用，上线前回退 flash）。

---

## 10. 测试计划与置信度更新

- P2：agent 级测试（effort 两态、压缩成功/回退、缓存命中/失效）+ config 解析测试。
- P3：tokens 单测（EN/CJK/混合/消息级）；中途检索（周期注入、去重、压缩后重建）；
  memory/graph 中文 FTS（trigram + LIKE 回退）；嵌入（确定性、余弦排序、hybrid 融合、
  provider=None 等价）；统一召回块纯函数。
- P4：coordinator 图工具注册（graph.enabled 开关两态）；desktop 前端类型/事件测试 +
  `make check-desktop`；config `[model_pointers]` 解析/merge 测试。
- 全量：`make check`（fmt + clippy + workspace tests + doc）+ PR CI（含 Windows；
  需并入 `agent/fix-windows-gitignore` 的 scanner 修复）。

**置信度**：P2 已实现并有测试（高）；P3 计量口径与检索设计（中高，官方文档口径 +
零依赖实现）；P4 桌面端 UI 形态（中，交互细节以可读性为准）；整体验收以 `make check`
与 CI 结果为准。

## 7. P2–P4 详细设计（执行版）

### 7.1 P2：高频决策经济学（已完成）

**每步 effort 动态路由**：`Agent::with_effort_routing(quick, high)` 装配双 provider；
循环内 `classify_quick_step` 判定上一条消息为正常（非 `Error:`）Tool 结果时走 `quick`
（reasoning disabled），首步/出错/回炉反馈走 `high`。CLI 在
`step_effort_routing=true` 时经 `step_effort_providers` 注入。机制为纯规则、零额外
LLM 成本，保证"机械续步不烧 reasoning token，关键决策给足推理"。

**观察压缩**：`observe_compress_*` 配置开启后，超阈值工具结果先经廉价模型
（compact/quick 指针）产出结构化摘要再入历史；原始结果保留在事件流。

**会话工具缓存**：只读工具结果按 `(工具名, 参数)` 哈希缓存，命中注入
`[cached]` 前缀；写类工具（write/edit/bash 等）执行后整表失效。代价可控
（`tool_cache_key` 为 FNV 哈希），收益集中在高频只读循环。

**设计取舍**：三个开关默认关闭，避免未校准前改变默认行为；数据驱动后可经
`config.example.toml` 一键开启。

### 7.2 P3：上下文与检索工程（已完成）

**真实 token 计量**：按 DeepSeek 官方口径（1 英文字符 ≈ 0.3 token、1 中文字符 ≈
0.6 token）实现 `core::tokens::estimate_text_tokens`，替代旧的 `字节数/4`。
压缩阈值、滑窗、子代理预算、delegate 截断统一换口径；截断侧仍用保守
`chars_for_tokens`（4 字符/token）保证不丢关键内容。中文场景偏差从 ~2.4 倍收敛
到 ±20% 以内（BPE 分词仍有模型差异，真实 usage 始终优先）。

**中途检索**：除会话起点与 L3 压缩后召回外，Agent 循环新增周期性子目标切换召回：
每 8 步且上一条为 Tool 结果时，以最近 User 消息为查询注入 `<recalled-memory>`，
带注入间隔防重。FTS5 查询为本地 SQLite，成本可忽略。

**记忆/图桥接**：`runtime/retrieval.rs` 把 MemoryEngine 命中与 GraphIndex 符号命中
合并为单一召回块（`## Recalled Context` + `## Recalled Symbols`），共用 token 预算；
graph 未启用或索引失败时静默降级为纯记忆召回。

**中文检索**：memory FTS5 主表保持 `porter unicode61`（ASCII 精确性），新增
`trigram` 副表覆盖无空格中文查询；双路查询按 id 去重取更优 bm25。短查询（<3 字符）
走 LIKE 回退，不 panic。

### 7.3 P4：产品化闭环（已完成）

**Coordinator 图工具**：`CoordinatorRunner::with_extension` 把 GraphHandle 注入执行器
ToolContext（与单 Agent 路径同机制）；CLI 在 `graph.enabled` 时把三个图工具注册为
**只读工具**交给规划器（`validate_plan_tool_boundary` 强制只读边界），执行器同样
可见；未启用时工具 schema 不暴露。

**桌面端验证事件**：`RunEvent::Verification` → `WireEvent::Verification`
（`kind="verification"`，含 command/passed/summary），桌面 Tauri 通道与 serve SSE
双路透传，前端会话流渲染 ✓/✗ 验证行。

**默认模板**：根目录 `config.example.toml` 提供 DeepSeek V4 推荐配置——
`main/quick/compact = flash`（高频决策），`task = pro`（重任务/规划，未上线时回落），
单价、P2 开关、`[verify]` 齐备；`setup.rs` 生成的默认配置附带同款注释模板。
