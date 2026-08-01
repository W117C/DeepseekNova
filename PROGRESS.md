# PROGRESS — TUI 设计功能完善（任务书执行）

## 系统提示词体系 + 后端审计 — 开工回执（2026-08-01）
- 理解的目标：主 agent 默认系统提示词（低成本高频决策引擎 + Observe→Plan→Tool→Verify→Reflect→Next Action，英文）默认启用且可配置覆盖；全链路子提示词统一；后端 22 crate 审计报告只报不修。
- 顺序：任务 0 基线 → 1 BACKEND_AUDIT.md → 2 prompts.rs+接入+4 类单测 → 3 PROMPT_DESIGN.md+子提示词对齐+契约测试 → 4 文档/反向验证/提交推送。
- 最大风险：with_appended(None) 语义改变（默认+追加）与 runtime 图检索提示词英文化；scanner/runtime 跨 crate 按 AGENTS.md §1 记录；子提示词改写不得破坏 JSON/章节/工具名机器契约。
- 基线证据（本轮实测）：feat/tui-complete@c19898d 干净；make check EXIT=0、638 通过；agent 108 单测+1 doctest 绿。

## TUI 视觉改造（参考 Codex CLI）— 开工回执（2026-08-01）
- 理解的目标：把 TUI 观感改成 Codex CLI 风格——语义配色（禁 Blue/Yellow/White/Black/DarkGray/Rgb）+ 干净底部面板（状态行多段着色、提示行、无 emoji 标题），全部可机判函数加单测，观感留领导亲验。
- 顺序：任务 0 基线 → 1 style_for 语义配色+映射单测 → 2 布局纯函数+底部四段 → 3 文档/反向验证/提交推送。
- 最大风险：改 style_for 影响全部渲染路径；输入框样式与光标位置在 draw() 内、不可单测，只靠布局/状态/提示纯函数锁行为；rg 禁止色检查是全量源码，任何残留都算失败。
- 基线证据（本轮实测）：feat/tui-complete @ 0b23765 工作树干净；cargo test -p deepseeknova-tui = 31+1 绿 0 ignored；make check EXIT=0。
- 决策：不改输入组件/不加依赖/不动既有测试断言（确认现有测试无断言旧颜色）；在 feat/tui-complete 上继续，PR #53 自动带上提交。

## 任务状态（TUI 视觉改造）
- [x] 任务 1：style_for 语义配色（User=Cyan+Bold、Agent=Magenta、Reasoning/Tool/ToolResult/System=Dim、Verification=Green/Red、Error=Red+Bold、Paused=Cyan），映射单测覆盖 9 种 LineKind。
- [x] 任务 2：布局四段（对话区/状态行/输入框/提示行），layout_constraints/status_segments/hint_text 纯函数 + 3 条单测；标题去 emoji；状态行 model=Cyan 其余 Dim；输入框边框 Dim、">"=Cyan+Bold、输入默认色；底部提示行全 Dim。
- [x] 任务 3：GUIDE/README/CHANGELOG 已更新；cargo fmt --check 过；make check EXIT=0；反向验证红（1 failed）→ 还原绿（35+1 全绿）；PTY 冒烟：新布局正常渲染、Esc 退出 code 0；BLOCKED.md 已补 diff 高亮待裁决项。
- 证据：cargo test -p deepseeknova-tui = 35 单测 + 1 doctest 绿 0 ignored；rg 禁止色（Blue/Yellow/White/Black/DarkGray/Rgb）零命中。

## 任务状态（系统提示词体系 + 后端审计）
- [x] 任务 1：BACKEND_AUDIT.md 已交付，22 crate 全覆盖（声明 vs 实现、测试数、结论、file:line 证据）；问题分级写入 BLOCKED.md；未改任何 crate 代码。
- [x] 任务 2：prompts.rs（DEFAULT_SYSTEM_PROMPT，英文，六阶段循环+决策引擎）+ agent.rs 接入（run_stream 默认注入、with_appended(None)=默认+追加）+ 4 类单测（阶段词/默认注入/追加/覆盖优先）；cargo test -p deepseeknova-agent = 112 单测+1 doctest 绿。
- [x] 任务 3：PROMPT_DESIGN.md（10 处改动清单+理由）；plan_mode/planner×2/delegate×4/review/compaction/scanner/graph hint（英文化）/compress_observation/verify 回炉文案全部统一六阶段术语；机器契约未动；新增 9 条契约单测（agent 119、scanner 15、runtime 26 全绿）。
- [x] 任务 4：GUIDE「System Prompts」节、README 提示词小节、CHANGELOG Added；cargo fmt 过；make check EXIT=0、651 通过（638+13）；反向验证红（1 failed）→ 还原绿（119+1+5 全绿）；跨 crate 记录见下。
- 跨 crate 协议记录（AGENTS.md §1）：预扫描=只改 scanner investigate.rs 与 runtime GRAPH_RETRIEVAL_HINT 常量区；备选路径=不改 PromptBuilder 装配 vs 直接在 agent 内做默认值（选后者，改面最小）；自检=三 crate 单测 + make check 全绿 + 反向验证。

## 开工回执（2026-08-01）
- 理解的目标：TUI 命令面与 REPL 对齐（/skills /mcp /raw /undo）、输入可编辑+可见光标、运行事件代际防串台、/resume 渲染历史、文档同步，全部带测试并过 make check。
- 顺序：任务 0 基线 → 1 输入编辑（纯单 crate）→ 2 代际 → 3 命令补齐（大头，跨 crate）→ 4 resume 渲染（跨 crate）→ 5 文档/反向验证/PR。
- 最大风险：/undo 与 resume 签名变更涉及跨 crate（tui+cli），按 AGENTS.md §1 推理专家协议执行；/undo 只许在 tempdir 测试，不许动仓库真实文件。
- 任务 0 证据：main@68fd58c；make check 全绿（tui 16 单测+1 doctest）；cargo metadata --locked 现为 LOCKED_OK。
- 存量失配：提交版 Cargo.lock 缺 async-trait→cli、deepseeknova-provider→tui 两条记录；cargo 运行已自动补录（git status 显示 M Cargo.lock）。书本预期 metadata --locked 先失败，因补录已发生改为通过——失配证据=提交版 lock 与 manifest 对比（git show 两文件核对）。

## 任务状态
- [x] 任务 0：基线核验
- [x] 任务 1：InputState + 光标（21 单测绿，clippy -D warnings 零告警）
- [x] 任务 2：RunSession 代际（23 单测绿）
- [x] 任务 3：/skills /mcp /raw /undo（TUI 31 单测绿；CLI 侧 tui_undo.rs 2 测试绿，tempdir 回滚验证 modified→unchanged）
- [x] 任务 4：/resume 渲染历史（SessionController::resume 返回 ResumedLine 列表，CLI 按 role 映射）
- [x] 任务 5 前半：GUIDE/TUI README/CHANGELOG 已同步；make check 全绿；cargo metadata --locked 通过（LOCKED_OK）；反向验证：改坏 skills 测试断言 → 红（0 passed; 1 failed）→ 还原 → 31 绿 + FMT_OK
- [x] 任务 5 后半：分支 feat/tui-complete，commit cbca2ef，push 成功，PR #53 已开（body 已修正）；CI/Desktop Build/Security 已在跑

## 交付物
- PR: https://github.com/W117C/DeepseekNova/pull/53
- 本地：feat/tui-complete @ cbca2ef，工作树干净；main 未动

## 追加修复（实测发现）
- Esc 退出后进程悬挂：stdin 读取线程阻塞在 event::read，tokio 停机等待 → 已改为 poll(100ms)+is_closed 轮询，Esc 干净退出（PTY 实测 code 0）。

## 任务 3/4 备注
- /undo 采用 trait 接法（备选 A）：TUI 定义 UndoController，CLI 在 crates/deepseeknova-cli/src/tui_undo.rs 用 CheckpointManager 实现，每次调用重新 load_from 磁盘，天然支持 &self 与多进程共享。
- /resume 的 StoredMessage.role 是 String（"user"/"assistant"/"system"/"tool"），按字符串映射，tool 归 System 展示。

## 跨 crate 协议记录（AGENTS.md §1 触发项）
- 错误预扫描（禁行区）：不改 core/provider/agent 公共 API；不改既有 16 条 TUI 测试断言（resume 签名变更的最小适配除外）；不对仓库真实文件执行 rollback；不新增外部依赖。
- 备选路径：/undo 接法 A=trait（TUI 定义 UndoController，CLI 用 CheckpointManager 实现，保持 TUI 不依赖 config）vs B=TUI 直接依赖 checkpoint+路径参数。选 A：依赖方向与 SessionController 一致，CLI 侧可单测。
- 自检：每项完成后 cargo test 单 crate，收尾 make check 全绿 + 反向验证红→绿证据。

## 决策记录（建议/偏离）
- 任务 0 的 metadata --locked 预期调整见上（补录已发生，非未做）。

## 任务书：CodeGraph 增强（动态分发 + trace/impact/explore）— 开工回执（2026-08-02）
- 理解的目标：graph crate 增加 Rust trait 动态分发桥（Dispatch 边：trait 方法 → 全部 impl 方法），新增 trace_code（多跳调用链）、impact_code（影响面聚合）、explore_code（按文件源码分组）三个只读工具并接入 runtime；文档同步；PR 提交。
- 顺序：任务 0 基线 → 1 动态分发（model+parser+store+graph 单测）→ 2 trace → 3 impact → 4 explore → 5 runtime 注册/提示词/文档/反向验证/PR。
- 最大风险：tree-sitter Rust 节点字段（已查 grammar.json 核实 impl_item.trait/type、trait_item.name、field_expression.field）；Dispatch 边加入后 PageRank 与邻居查询行为变化；跨 crate 改动按 AGENTS.md §1 记录。
- 基线证据（本轮实测）：main@dd373e1 干净；make check EXIT=0；graph 19 单测绿 + 1 存量 ignored；tools 45+12+7 绿。
- 关键决策（偏离白名单的替代路）：新工具注册点 tools/src/lib.rs 不在白名单，改为 graph_tools.rs 导出 graph_query_tools()，runtime 在 graph.enabled 时注册（关闭时不注册 = 行为等价于禁用，且不动 schema 预算测试）。

## 任务状态（CodeGraph 增强）
- [x] 任务 1：动态分发（EdgeKind::Dispatch；解析 function_signature_item（trait 方法）与 impl Trait for Type 方法；raw_trait_methods/raw_impl_methods 事实表 + schema v3 强制重解析；全局重建 Dispatch 边：trait 方法 → 全部同名 impl 方法）。graph 单测 24 绿（+5，含 dyn 调用桥接两个候选的 fixture）。
- [x] 任务 2：trace_code（store.trace_paths：DFS 深度上限 6、路径上限 100、超限 truncated 标记；callers 归一为源→目标；tools 测试 a→b→c 带行号）。
- [x] 任务 3：impact_code（反向路径按 文件×符号×路径 聚合 + total 统计；tools 测试 2 文件 2 路径）。
- [x] 任务 4：explore_code（按文件分组、区间合并、行号源码 / skeleton；tools 测试跨文件分组）。
- [x] 任务 5：runtime 注册 graph_query_tools（graph.enabled 时）+ 两个注册测试只加断言；GRAPH_RETRIEVAL_HINT 补 4-6 条；GUIDE / graph README / CHANGELOG 已更新；cargo fmt + make check 全绿；反向验证：改坏 trace_truncates 断言 → 红（1 failed）→ 还原 → 绿（graph 24+1 存量 ignored、tools 50+12+7）。
- 跨 crate 协议记录（AGENTS.md §1）：预扫描=不碰 core/agent 公共 API、不改既有断言（runtime 两测试只加新工具断言）；备选路径 A=改 tools/src/lib.rs 的 all_builtin 注册表（路径不在白名单）vs B=graph_tools.rs 导出 graph_query_tools() 由 runtime 按 graph.enabled 注册（白名单内、行为等价、不触及 schema 预算测试）——选 B；自检=三 crate 测试 + make check + 反向验证红→绿。

## 交付
- 分支 feat/codegraph-trace，PR #54 已合入 main（2026-08-02 验收后合并）

## 任务书：Context7 文档检索（context7_docs）— 开工回执（2026-08-02）
- 理解的目标：新增只读工具 context7_docs（库名+主题 → Context7 文档片段），域名固定 context7.com、NetworkAccess 把关、错误全转友好提示；runtime 常驻注册；文档同步；PR 提交。
- 顺序：任务 0 基线 → 1 docs_tools.rs（URL 构造/解析/截断/域名校验纯函数 + 本地 TcpListener 端到端）→ 2 lib.rs 两行 + runtime 注册 + 断言 → 3 文档/反向验证/分支 PR。
- 最大风险：本地 HTTP 端到端测试首次引入（tokio net 已在依赖内）；schema 预算测试不可碰 → 工具不进 all_builtin；PR #54 未合入导致基线数字与书不一致（见 BLOCKED.md）。
- 基线证据（本轮实测）：main@dd373e1 工作树干净；make check EXIT=0；tools 45+12+7 绿 0 ignored；runtime 26 绿；schema 预算 4624/5000。
- 决策：注册走 runtime（与 graph 工具同款），tools/src/lib.rs 只加模块声明与 pub use；验收后与已合入 #54 的 main 合并，解决重叠文件（runtime/GUIDE/CHANGELOG/PROGRESS/BLOCKED）冲突。

## 任务状态（Context7 文档检索）
- [x] 任务 1：docs_tools.rs（Context7DocsTool + search/context URL 构造、first_result 解析、UTF-8 安全截断、域名固定纯函数 + 本地 TcpListener 端到端 3 条：成功/空结果/HTTP 500）；共 9 条新测试，全绿。
- [x] 任务 2：tools/src/lib.rs 仅加 pub mod docs_tools; 与 pub use；runtime 在工具注册区常驻 register(docs_tools())，disabled 过滤沿用 register 闭包；memory 注册测试追加 context7_docs 断言（只加不改）；schema 预算测试原样且绿。
- [x] 任务 3：GUIDE Library Docs 节（参数/来源/禁用方式）、tools README、CHANGELOG Added；cargo fmt + make check EXIT=0；反向验证：改坏 first_result 断言 → 红（1 failed）→ 还原 → 绿（tools 54+12+7、runtime 26）。
- 跨 crate 协议记录（AGENTS.md §1）：预扫描=不碰 security/config/既有断言（memory 测试仅追加断言）、不新增外部依赖（reqwest/url/serde_json 均在 tools 依赖内）；备选路径 A=进 all_builtin 列表（实测 schema 预算 4624+约 400 > 5000，预算测试不可改）vs B=runtime 常驻注册（任务书拍板）——选 B；自检=单 crate 测试 + make check + 反向验证红→绿。

## 交付与验收记录
- 分支 feat/context7-docs，PR #55 已合入 main（2026-08-02 验收后合并）；#54、#55 明卷+暗卷+CI 全绿，远端/本地分支已删。

## 任务书：反思→修复闭环 — 开工回执（2026-08-02）
- 理解的目标：P1 验证与 B3 审查失败回炉前插入显式 LLM 反思（JSON root_cause/fix_plan/lesson），回炉消息前置反思，lesson 经 LessonHook 落 core 记忆；`[agent] reflect_on_failure` 默认 true 可关。
- 顺序：任务 0 → 1 core record_reflection_lesson → 2 agent reflection.rs + 两分支接入 → 3 config+runtime 装配 → 4 文档/反向验证/PR。
- 最大风险：agent.rs 回炉分支是循环热路径，改动只许「插入反思+换消息」，不许动循环语义；main 上 review::extract_json 私有 → reflection.rs 自带等价实现；stub 空文本不会 Done，装配测试按流结束断言。
- 基线证据（本轮实测）：main@2c71284 干净；core 83+2+1、agent 119+1、config 20+18、runtime 26 全绿 0 ignored；make check 670。

## 任务状态（反思→修复闭环）
- [x] 任务 1：core record_reflection_lesson（Skill、tags=[reflect,lesson]、source=reflect-loop、哈希去重+脱敏）+ 2 条单测。
- [x] 任务 2：agent reflection.rs（prompt/parse/run_reflection/compose_retry_message，自带 extract_json 等价实现）+ Agent reflect_settings/lesson_hook 字段与构造器 + P1/P3 两个回炉分支接入 reflect_retry + 5 条单测。
- [x] 任务 3：AgentConfig reflect_on_failure（默认 true）/reflect_model/reflect_max_chars（4000）+ merge + 测试；runtime 装配（provider 回落 main + memory 启用时挂 LessonHook）+ 装配测试 1 条。
- [x] 任务 4 前半：GUIDE 反思节、CHANGELOG 已更新。
- [x] 收尾：cargo fmt + make check EXIT=0（679 通过）；反向验证红（1 failed）→还原绿（core 85、agent 124、config 21+18、runtime 27）；分支 feat/reflect-loop 已推送。

## 交付
- 分支 feat/reflect-loop，已推送 origin；PR: https://github.com/W117C/DeepseekNova/pull/59
