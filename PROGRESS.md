# PROGRESS — TUI 设计功能完善（任务书执行）

## 发布 v0.5.0（2026-08-08）

- **发布**：tag v0.5.0 推送触发 release.yml；CI 产 4 平台二进制（linux
  x86_64/ARM64、macos ARM、windows x86_64），**Intel Mac（x86_64-apple-darwin）
  因 GitHub macos-13 runner 无限排队，改由本地交叉编译补齐**（`rustup target
  add x86_64-apple-darwin` + cargo build --release，Mach-O x86_64 验证通过）。
- **GitHub Release v0.5.0 已公开**：5 平台二进制 + 完整 checksums.txt（资产命名
  与 install.sh 契约一致）；`install.sh` 无参解析 latest → v0.5.0，端到端实测
  下载/校验/安装/`--version` 0.5.0 全通过。
- **一键安装**：`curl -fsSL .../install.sh | sh`（macOS/Linux）、
  `irm .../install.ps1 | iex`（Windows）。
- **遗留**：CI 的 macos-13 job 仍排队（资产已齐，不影响用户，可后续取消）；
  install.ps1 Windows 实机冒烟待 Windows 环境。
- **状态**：CHANGELOG Unreleased 冻结为 [0.5.0] 节；本次提交仅文档。

## 并行优化三批（2026-08-07，调研驱动轮）

- **前置**：七子代理并行调研（竞品对标 / 执行引擎 / 安全域 / 接口面 / 代码健康 /
  发布就绪 + 综合）产出 P0×9 / P1×12 / P2×8 路线图；P0 级安全 claim 父级逐条
  抽查实锤。四项决策用户拍板：默认安全姿态（权限门控默认开+横幅+`--secure-defaults`）、
  Ask 无 responder 默认 deny、i18n 双语框架（英文默认+中文可选）、Tauri 壳降 P2。
- **第一批**：B1 安全硬化（`/dev/tcp` 拦截、重定向逐跳校验、move_file 双路径守卫、
  记忆/待办能力门、审计 JSONL 落盘、sanitize 扩展、资源限额统一、reqwest 共享）；
  A3 MCP 协议层测试补齐（0→28 个回放测试 + 写通道有界化）；A6 checkpoint 快照
  上限 + 增量持久化。
- **第二批**：B2 serve 加固（CORS 收窄 loopback-only、done 事件带 session_id、
  API key 文档统一、serve 契约文档同步）；B3 默认姿态（权限门控默认开、
  `--secure-defaults`、启动横幅、Ask 无 responder 默认 deny、runtime 审计切 JSONL、
  交互 REPL 审批 responder）。
- **第三批**：A1 agent 热路径（快照复用 7→1~2 克隆/步 + Memory 零拷贝接口 +
  MetricsGuard 误切片修复）；A2 core 死模块清理（identity/prefix/progress/plugin
  20 文件删除 + 依赖清理 + DESIGN 同步）。
- **i18n 受阻**：双语框架子代理因 API 402 余额不足中断（迁移约 30%，词表
  keys.rs 914 行已建）；半成品回退，词表备份 `/tmp/deepseeknova-i18n-backup/`，
  待余额恢复后重启（BLOCKED.md 已记录）。
- **验证**：每批子代理域测试全绿；两轮 `make check` EXIT=0（复检前全 workspace，
  含 TUI 提交后与三批合流）。审计测试产物 `audit.jsonl` 残留已修（.gitignore 补
  `**/.deepseeknova/security/`）。
- **状态**：未提交（三批成果 + CHANGELOG/PROGRESS/BLOCKED/文档待统一提交）。

## 并行优化第四批（2026-08-08，调研驱动轮续）

- **前置**：三批成果已提交 b174d72（67 文件 +3647/-1417），工作树干净后启动。
- **P1-1 AGENTS.md onboarding**：`init` 默认生成行业标准 `AGENTS.md`（已被
  Claude Code/Codex/opencode 识别），`--legacy` 回退 DEEPSEEKNOVA.md；引导式
  Next steps；46 测试 + 真实二进制冒烟。
- **P0 发布就绪收尾**：scanner/graph/metrics 三 crate 补 description/keywords/
  workspace 依赖；README 截图占位改文字说明；SECURITY 版本表 latest-minor 策略。
  **实证：版本须 bump 0.5.0**（crates.io 0.4.0 被旧快照占用 19/22，dry-run 实测
  metrics 编译失败）——bump 待用户确认后执行。
- **A7 provider 接线**：工具 schema 缓存（ToolSchemaCache）、temperature 接线
  （ModelRouter→请求体）、anthropic reasoning_effort 门控、telemetry 死代码删除；
  60 测试。
- **i18n 双语框架（重启成功）**：上轮 402 中断后重启，词表 100% 复用备份
  （keys.rs 当时 236 键 + mod.rs；截至 2026-08-10 为 257 键），190 处中文迁入词表，162 测试；**全程每 3-5 文件
  保持可编译**（吸取上轮半成品教训）。词表结构即 Tauri 壳 P2 契约（桌面脚手架已
  随 c10fec3 移除，Tauri 壳仍为 P2 计划项，词表契约暂未消费）。
- **验证**：`make check` EXIT=0（复检发现 i18n/mod.rs doc 交叉引用私有项警告，
  已修，doc 零警告恢复）。
- **状态**：待提交（34 文件；版本 bump / gh repo edit / docs/superpowers 处置
  等发布动作待用户裁决）。

## 并行优化第五批 + 发布准备（2026-08-08）

- **发布准备**（0d9ccc4 提交）：版本 bump **0.5.0**（workspace.package + 23 内部
  依赖 + 20 docs URL；crates.io 0.4.0 被旧快照占用实证必须）；gh repo edit
  description 去 Desktop + topics 清 tauri/goap；docs/superpowers 加 internal
  planning archive 标注。
- **P1-6 权限模式 + 工作区信任**：`[permissions] mode` 三档预设（plan/
  accept_edits/auto，`Option` 缺省 None 保持旧行为防安全回归）+ TUI Ctrl+P 循环/
  状态栏/审批浮层/`/mode` 命令；`TrustStore`（trusted.toml，untrusted 项目层
  allow 降级 ask）+ 首进信任确认浮层。**CLI 已补接线**（gate 与 agent 同实例 +
  CliTrustController 委托 TrustStore，父级收尾完成）。
- **P1-2 eval 分级升级**：`min_score`/`dimension_min`（中文别名）/`cost_max`/
  `rounds` AND 语义断言 + `--require-min-score`/`--require-dimension` CI 门槛
  （退出码 1/2/3 区分条目/门槛失败）+ 评分卡内存钩子不污染 metrics 目录。
- **验证**：`make check` EXIT=0（复检修 2 处：P1-6 trust 测试 clippy
  field_reassign_with_default、P1-2 `dimension.<name>` doc HTML 标签警告）。
- **状态**：待提交。

## 并行优化第六批（2026-08-08）

- **P1-5 子代理升级**（agent crate 独占）：markdown 前端文件（agent_manifest.rs，
  `---` 头块 + `.deepseeknova/agents/*.md` 扫描，与 TOML 预设双通道兼容）；
  @-mention（mention.rs，词边界感知防邮箱误拆）；放开禁递归（recursion.rs，
  深度上限默认 3，DelegationSink/DelegateDepth/RecursiveDelegateTool，超深
  优雅降级）；per-agent 模型/权限（ModelResolver + AgentPermission 交集）。
  303+1+4 测试。**父级串联已闭环（2026-08-10）**：`build_delegate_engine`
  按 `allow_recursion` 装配 `RecursiveDelegateTool`（sink = 引擎自身）并注入
  每层真实深度；config 接入 agents 目录/深度/per-agent 配置；主对话
  @-mention 入口（`MentionAwareRunner` + CLI REPL/TUI 装配），见 REVIEW.md
  后续轮。
- **P1-9 会话 UX**：`/rename` 会话命名（titles.json，title 优先回退 id）；
  会话级 checkpoint `/checkpoint save|list|rollback`（SessionCheckpointManager，
  对话快照 + 回退重写 agent history；save_with_files API 预留文件部分）。
  16 i18n 新键。checkpoint 25 / tui 180 / cli 73 测试。
- **P1-11 记忆用户面**：`memory list/edit/delete/replay`（浏览过滤分页 /
  编辑保留 lifecycle 重嵌 / 删除二次确认 / 与 recall 同源的检索分解回放）。
  core 记忆引擎最小新增接口（search_breakdown/edit/replay，hybrid 总分
  严格不变）。core 146 / cli 71 / tools 117 测试 + 真实 CLI 冒烟。
- **验证**：`make check` EXIT=0（复检修 4 处 P1-5/P1-9 doc 注释：agent_manifest/
  recursion 冗余链接、tui SessionCheckpointManager 未解析链接）。
- **状态**：待提交。

## 并行优化第七批（2026-08-08）

- **P1-8 用户级 hooks**（core/config/agent/runtime）：`[hooks]` 五事件
  （tool_before/after、session_start/end、failure）+ JSON 协议 + fail-closed
  （tool_before 任一命令非 0/超时/裁决拒绝即阻止执行，内部链之后叠加）；
  failure 挂 MetricsGuard emit（Paused/异常触发）。无 hooks 零进程开销。
  core 155 / config 65 / agent 317 / runtime 59 测试。
- **P1-7 exec 审计模式**（security/permission/cli）：`audit <cmd>` 预执行分类
  预览（只读放行/Ask/硬拒 + 命中规则 + 形态 + 建议，md/json）；CommandAudit 与
  分类器同源、gate preview 与 check() 共用 preflight+finalize（一致性测试
  背书）、只计算不执行（不写缓存/限流/审计）。security 119 / permission 57 /
  cli 89 测试 + 端到端实测。
- **P2-2 graph 语义检索**（graph 独占）：复用 core EmbeddingProvider trait
  （记忆侧同款融合 w*bm25+(1-w)*cos + fail-open + 同 blob 编码）；写入即嵌入，
  SCHEMA 增量不 bump 版本（旧索引零破坏）；open_with_embedder/
  search_hybrid[_breakdown]。49 测试。**runtime 装配待父级**（从 memory config
  构造 embedder 喂 graph）。
- **验证**：`make check` EXIT=0（复检修 graph store.rs:889 区间记号 `[0,1]`
  doc 链接警告）。
- **状态**：待提交。

## 并行优化第八批（2026-08-08，功能接线串联轮）

- **子代理递归装配（P1-5 遗留）**：`[delegate] allow_recursion` + `max_depth`
  （默认 3）；`build_sub_agent_runner` 装配 RecursiveDelegateTool +
  set_delegation_sink（SubAgentRunner 增 Clone，克隆体共享 sink 槽）；默认
  false 保持禁递归。engine 路径递归深度传播待 Agent 主循环注入
  DelegateDepth（已知后续）。
- **graph 语义检索接线（P2-2 遗留）**：runtime 从 `[memory] embedder="remote"`
  复用 RemoteEmbedder 喂 `GraphIndex::open_with_embedder`，fail-open 回落 FTS。
- **i18n CLI 接线**：`[ui] lang`（en/zh 及别名）→ `TuiRunner::with_lang`，
  优先于 `DEEPSEEKNOVA_LANG` env。
- **验证**：`make check` EXIT=0（修 fmt 签名格式、UiConfig derivable_impls、
  2 处 DelegateConfig 字面量补新字段）。
- **状态**：待提交。

## 并行优化第九批（2026-08-08，P2 续）

- **P2-3 MCP streamable HTTP**（mcp 独占）：自动探测 legacy SSE vs streamable
  HTTP（增量读取修常开 SSE 阻塞）；Mcp-Session-Id 完整生命周期（捕获/回发/
  跟随/404 过期清空）；protocolVersion 协商（2025-06-18/03-26/2024-11-05，
  -32602 降级重试）；lib.rs 声明修正。60 测试。
- **P2-4 会话级花费上限**（config/provider/agent）：`[budget] max_total_cost_usd`
  + CostLedger::total_usd + CostBudget（from_router）+ 主循环独立检查点（超限
  Paused reason 保留 `budget:` 前缀对齐 CLI 退出码，token 预算路径未改）。
  **CLI 已装配**（父级 build_agent_in 注入）。跨会话累计留待后续。
- **P2-6 缓存承诺真实化**（core/context/docs）：README 撤稿（API 级缓存真实 +
  会话级命中率 [规划中]）；core 字段 doc 诚实化（serde 保留无 BREAKING）；
  context builder 标注库级 API。另修文档漂移：README 测试数 1251→1571、
  README_EN Reasoning Effort 4-level→3-level。
- **验证**：`make check` EXIT=0（含父级补的 CLI 成本装配）。
- **状态**：待提交。

## P2-5 收尾（2026-08-08）

- **RemoteEmbedder async 化**：`EmbeddingProvider::embed_async`（default
  spawn_blocking 桥接，不阻塞 worker；Arc<Self>+String 接收器 + Send+'static
  boxed future 保持 dyn-compatible），同步 embed 保留零破坏。RemoteEmbedder
  真实 async + 移除独立 runtime 字段（修 async drop runtime panic 潜在 bug，
  同步路径改共享 runtime）。provider 65 / core 153+2+1 / graph 49 测试。
  生产调用点迁移清单（graph/memory 4 处）留待后续。
- **验证**：`make check` EXIT=0。
- **状态**：待提交。

## 并行优化第十批（2026-08-08，P2 大项收尾）

- **P2-7 worktree 并行会话**（cli + GUIDE）：`worktree new|list|switch|delete|
  clean` 子命令 + git worktree 封装（主根 `.deepseeknova/worktrees/`、非 git
  检测、分支名校验、`/var`→`/private/var` canonicalize）+ 会话隔离语义
  （worktree 内 graph/memory/审计按 workspace_root 落盘天然隔离）。11 测试 +
  e2e 冒烟。cli 100 测试全绿。
- **P2-1 沙箱平台无关增强**（sandbox + config）：`[sandbox] network_allow_domains`
  域名白名单配置接口 + `NetworkPolicy` 类型（warn-not-fail 校验）。诚实约束：
  seatbelt/bwrap 仅支持整网开关（域名级过滤标注后续）；Windows Job Object/
  AppContainer 后端不在 macOS 写（无法验证运行时隔离），方案文档落 sandbox
  crate doc。sandbox 22 / config 53 测试。
- **验证**：`make check` EXIT=0（复检修 cli worktree doc 3+2 处 HTML 标签/断链
  警告）。
- **状态**：待提交。

## 收尾小项（2026-08-08）

- **MCP 会话过期自动重连**：404+session 头 / 空 session id → 自动重发
  initialize → 重试原请求一次；限 1 次防循环；并发双检锁 + generation 仅一次
  重连（8 并发测试无 panic）。mcp 65 测试。
- **embed_async 工具边界迁移**：remember/recall 工具路径 spawn_blocking 移出
  worker（线程 id 回归测试）；graph refresh 确认在 blocking 池保留同步；
  **graph search_hybrid 证实无生产调用方**（语义检索死路径，待产品决策）。
  core 156 / graph 49 / tools 118 测试。runtime 同步闭包 3 处留待 agent API
  改造。
- **engine 递归深度传播**：Agent 主循环注入 DelegateDepth(1)（根恒 1）；
  DelegateTool 读扩展传 run_at_depth(depth+1)，schema 去掉 no re-delegation；
  端到端引擎递归深度链 1→2→3 测试 + 超深守门精确验证。agent 328 / tools 121
  测试。引擎子代理自身深度注入留待 runtime。
- **验证**：`make check` EXIT=0。
- **状态**：待提交。

## graph 语义检索接线（2026-08-08，产品决策项）

- **决策（父级定）**：`search_code` 接通 `search_hybrid`——P2-2 的语义检索此前
  无任何工具调用（死路径），按"真的有用有效"标准接通。
- **实现**：`GraphIndex::search_best`——有 embedder（`[memory] embedder="remote"`
  + key）→ hybrid（0.5*bm25+0.5*余弦）；无 → **逐字节委托 search**（等价性测试
  锁定 4 组查询零回归）。工具侧 lock+检索 经 spawn_blocking 移出 worker
  （hybrid 查询嵌入 HTTP 最长 30s）。语义只对显式配置嵌入的用户生效。
- **验证**：`make check` EXIT=0；graph 50 测试（含 search_best==search 等价）；
  graph_tools 13 测试全过。
- **状态**：待提交。

## 一键安装（2026-08-08，发布就绪）

- **install.sh + install.ps1**（仓库根新建）：`curl | sh` / `irm | iex` 一条命令
  安装——GitHub Releases 下载预编译二进制 + 平台自动检测（macOS Intel/ARM、
  Linux x86_64/ARM64、Windows x86_64）+ **SHA256 checksum 校验**（失败即删并
  报错）+ 默认 `~/.local/bin`（`INSTALL_DIR` 可覆盖）+ 未发布平台/无效版本清晰
  报错。**install.sh 经 v0.4.0 真实资产端到端实测**（下载/校验/安装/运行全过）。
- **release.yml 矩阵 3→5 平台**：新增 macos-13（x86_64-apple-darwin，Intel 原生
  runner）+ ubuntu-24.04-arm（aarch64-unknown-linux-gnu，ARM 原生 runner；
  tree-sitter/rusqlite bundled C 依赖需原生构建）。命名与 install.sh 契约一致。
- **README 安装节**：中英同步一键安装（推荐）+ binstall 备选 + 源码备选；
  诚实注明 v0.4.0 仅 3 平台、v0.5.0 覆盖 5 平台。
- **复检发现并修复**：CLI `#[command(version="0.4.0")]` 硬编码 → 
  `env!("CARGO_PKG_VERSION")`（`--version` 现输出 0.5.0，与 workspace 一致）。
- **验证**：`make check` EXIT=0；父级复跑 install.sh 端到端全过。
- **状态**：待提交；tag v0.5.0 触发真实构建由用户拍板。

## 日常体验包 + 审查修复（2026-08-07，非任务书轮）

- 功能：web_search（ddg / tavily / bing / searxng）、lsp_diagnostics（写后
  自动注入诊断）、`[agent] auto_route`（每 run 决策一次、按 run 隔离）、
  serve durable runs（`/v1/runs` + resume + 原子 claim）、Windows 运行时
  沙箱警告。
- 审查：`ocr delegate preview` 61 可审查文件；7 个发现（LSP 空诊断等满超时、
  auto 路由缓存跨并发 run 串扰、SSE 断开取消任务并误标 Done、web_search 重定向
  SSRF、未知 provider 静默回落 DDG、LSP 缺 FileRead 门控、resume 竞态）全部
  修复并补回归测试。
- 验证：`make check` EXIT=0（fmt / clippy -D warnings / 全 workspace 测试 /
  doctest / doc 零警告）。
- 状态：未提交、未 push、无 PR；分支 feat/semantic-retrieval 继续承载全部
  未提交改动。
- 遗留（已做）：LSP 端到端 fake-server 测试（空诊断 1.5s 宽限内返回，不再
  等满超时）；最小 evals（`eval` 子命令 + JSONL `must_contain`）；ACP 适配器
  （`serve --acp`，initialize/session/new/prompt/cancel/close + 会话多轮历史
  + `Ask` fail-closed）。
- 清理：`docs/experiments/`、`scripts/experiments/` 与
  `docs/superpowers/mockups/` 按用户批准全部删除（未跟踪目录直接删，mockups
  已 git rm）。
- 验证：`make check` EXIT=0（含新增 ACP 往返单测、eval 3 单测、LSP 空诊断
  e2e）；`serve --acp` 进程级冒烟通过（initialize / session/new / close 协议
  响应正确、按 cwd 建 agent）；真实 LLM 冒烟因 `DEEPSEEK_API_KEY` 缺失
  blocked，未伪造凭据。
- 状态：已提交（2 个 commit）、已推送、PR #72（分支 feat/semantic-retrieval）。

## 安全边界收尾二轮（2026-08-06，审查修复轮）

非任务书轮：对上一轮收尾后的工作区再做一次审查 + 修复 + 知识收尾。

- 审查：`ocr delegate preview` 55 可审查文件；2 critical + 3 high + 1 medium +
  1 low，详见 crates/REVIEW.md 二轮分节。
- 修复：gh api 隐式 POST、建议规则通配符放大（Rule.exact）、file `-C`、
  裸 printenv、shell 组合 Dangerous→NotReadOnly、coordinator history 上限、
  删除 dbg_status_test.rs 死文件。
- 验证：`make check` EXIT=0；security 103 / permission 34 / tools 69+12+7 /
  agent coordinator 全绿。
- 文档：GUIDE（权限语义/TUI 快捷键/折叠/审批浮层）、CHANGELOG Unreleased、
  REVIEW/CLOSEOUT 追加分节、AGENTS.md §5 归档 3 条。
- 状态：未提交、未 push、无 PR；docs/experiments/ + scripts/experiments/
  已于 2026-08-07 按用户批准删除。

## 安全边界收尾（2026-08-06，审查修复轮）

非任务书轮：对工作区未提交的安全边界改动做审查 + 修复 + 知识收尾。

- 审查：`ocr delegate preview` 17 可审查文件；3 high + 5 medium + 1 flaky，
  详见 crates/REVIEW.md 本轮分节。
- 修复：readonly 分类器写形态误放行（date/hostname）、gh token `=true` 绕过、
  路径 `..` 逃逸、deny 建议误导、bubblewrap FullAccess、journalctl 漏拒、
  沙箱工作区默认可写、子代理无 gate 行为一致性、并行测试临时目录撞名。
- 验证：`make check` EXIT=0（fmt / clippy -D warnings / 1108 passed /
  doctest / doc）；修复前复现用例独立 harness 实测翻转。
- 文档：GUIDE sandbox 节、README/README_EN（权限模型/工具数/测试数）、
  DESIGN §九、CHANGELOG Unreleased、SECURITY.md、AGENTS.md §5 防错清单、
  crates/CLOSEOUT.md 本轮六事实面。
- 状态：未提交、未 push、无 PR（提交/推送由用户决定）。

## 任务书：记忆语义检索（embedder）最小闭环（2026-08-05，dev-loop 轮）— 任务状态
- [x] 任务 0：基线核验（make check EXIT=0；feat/memory-lifecycle@e941f14 干净；
      core 132 / agent 231 / provider 35 / runtime 52 / cli 32 / config 33+18 /
      tools 66+12+7）。
- [x] 任务 1：store search_hybrid_with_weight（FTS 基数改纯 bm25 消除双重计权；
      最终分 = 0.5*bm25归一 + 0.5*cosine - rank_weight*lifecycle_penalty；
      search_hybrid 委托新方法）+ 2 测试（语义独有命中、weight=0 组合回归）。
- [x] 任务 2：provider RemoteEmbedder（embeddings.rs：独立 tokio runtime block_on、
      from_parts/from_memory_config、try_memory_embedder fail-open）+ 5 测试
      （本地 HTTP 端到端 ×3、config/env 校验、try 回落）。
- [x] 任务 3：config 增 embed_base_url / embed_timeout_secs（默认
      https://api.openai.com/v1 / 30）+ merge + 2 测试（合并保留/显式覆盖）。
- [x] 任务 4：engine open_with_embedder / open_in_memory_with_embedder（旧入口
      委托 None）；remember/record_task/record_knowledge 写入即嵌入（同模型跳过）；
      recall 有 provider 走 hybrid；backfill_embeddings（跳过 archived，返回
      (attempted, ok)）；stats.embedded + 3 测试（写入嵌入+语义命中、fail-open、
      回填计数+stats）。
- [x] 任务 5：runtime/CLI 装配 try_memory_embedder（缺 key 回落 FTS，runtime 测试
      1 条）；CLI memory stats 输出 embedded=、memory embed-backfill；tools 语义
      命中测试 1 条。
- [x] 任务 6：GUIDE 记忆节 + CHANGELOG Added + BLOCKED 两处语义检索转已做（代码图
      侧留待裁决）；cargo fmt；make check EXIT=0（0 failed / 2 既有 ignored）；
      反向验证红（1 failed）→ 绿（1 passed）；分支 feat/semantic-retrieval 待提交。
- 跨 crate 协议记录（AGENTS.md §1 触发）：预扫描=不动 core 既有公开 API 签名
  （新增 open_with_embedder / search_hybrid_with_weight，旧入口委托）、不改既有
  测试断言、不加外部依赖（复用 provider reqwest/tokio）；备选路径 A=core 直接加
  reqwest 依赖 vs B=RemoteEmbedder 放 provider（已有 reqwest）+ engine 收
  Arc<dyn EmbeddingProvider>——选 B；自检=单 crate 聚焦测试 + make check +
  反向验证红→绿。

## CLI 冒烟证据（2026-08-05 实测）
```
$ deepseeknova-cli memory stats
total=0 embedded=0 recall_hit_rate=0.00 reinforce_ratio=0.00 stages= archived=0
$ deepseeknova-cli memory embed-backfill
embed-backfill: attempted=0 ok=0
```

## 任务书：Protocol 增强收尾 + Graph Go 语言（2026-08-05，dev-loop 双域并行）— 开工回执
- 理解的目标：protocol 域=能力包（2026-08-05 已合入）仅剩 2 个未落地项收尾（task_rate
  指标 first_pass/retry_rounds、fitness record_use 回填）；graph 域=新增 Go 语言支持
  （tree-sitter-go 0.25.0，5 个分派点分支 + go.mod 外部依赖识别）。
- 顺序：两域并行（地界零重叠）；每域 任务 1→n 独立推进，父级统一收尾。
- 最大风险：protocol 域跨 agent+metrics+runtime+cli 四 crate（AGENTS.md §1.1 触发，按
  推理专家协议记录）；graph 域 tree-sitter-go 0.25 grammar 节点名需实测（Go 的
  function_declaration/method_declaration/import_spec 等）。
- 基线证据（2026-08-05 实测）：feat/memory-lifecycle@68fb094 工作区干净；workspace 全绿
  0 failed（2 既有 ignored：graph self_index、provider reasoning protocol）；核心测试数
  agent 231、core 132、graph 32、runtime 48、config 33、metrics 17、skills 24、cli 32。

### G 域回执（graph worker，2026-08-05）
- 理解：graph 新增 Go 语言支持——Lang::Go + tree-sitter-go 0.25.0（书内唯一新依赖），
  五个分派点分支 + GO_SRC fixture + go.mod 外部依赖 + deps_code 提示语 + 文档。
- 顺序：任务 1 grammar 实测→解析接入 → 2 Go fixture → 3 go.mod → 4 文档/收尾/提交。
- 最大风险：tree-sitter-go 0.25 节点名/字段名凭猜出错（必须先探测实测）；SCHEMA_VERSION
  =4 保持不动；`fn go(&self)` 与 search("go") 既有语义测试不得误改。
- 基线证据：graph 32 unittests 全绿；Cargo.lock 无 tree-sitter-go；工作区干净。
- grammar 实测（tree-sitter-go 0.25.0，探测测试实测，已删）：
  function_declaration(name=identifier)/method_declaration(name=field_identifier,
  receiver=parameter_list)/type_declaration>type_spec(name+type 字段)/struct_type>
  field_declaration_list/interface_type>method_elem（**非 method_spec**，无 body 字段）/
  import_declaration>import_spec_list>import_spec(path=interpreted_string_literal)/
  call_expression(function=identifier 或 selector_expression(field=field_identifier))/
  单行 import "fmt" 也是 import_declaration>import_spec。据此：Go 实体=function/
  method/type_declaration（判 type 字段→Struct 或 Trait），import 三态=本地相对
  →File、stdlib/第三方→External（grammar 无法区分后两者，fixture 三态都覆盖）。
- 任务 1/2/3 已完：Lang::Go + from_path(".go") + language() 映射；entity_kind 加 node
  参数（Go type_declaration 判 struct/interface）、is_import(import_declaration/
  import_spec)、callee_name 加 selector_expression、extract_signature 沿用 "{"（Go 同
  Rust/JS 风格，实测 func 签名到 { 截断正确）；import 分支 Go 三态（相对→File、
  其余→External）；GO_SRC fixture + 4 测试（entities/calls+imports/三态/refs）；
  store：is_manifest 加 go.mod、parse_go_mod_deps（块式+单行 require）、2 测试
  （解析 + Go 项目端到端 external_deps）；tools deps_code 提示语补 go.mod + 1 测试。
  graph 32→38、tools deps_code 3→5；cargo fmt 我的文件全过（对方 runtime 未 fmt）。
- 阻塞观察：make check 首跑被对方 metrics 半成品 E0063 卡住（对方已修）；二跑 metrics
  过了但轮到 tools 缺 #[tokio::test]（我修了）；现剩 runtime fmt diff（对方文件，等待
  对方 fmt 后重试 make check）。

### P 域开工回执（Protocol 增强收尾，执行者 2026-08-05）
- 理解的目标：task_rate（Scorecard 扩展 first_pass/retry_rounds，从 DiagnoseReport.failures
  推导，serde default 向后兼容）+ record_use 回填（recall 注入技能名汇入 session_skills →
  CLI 传集合 → fitness 记 use+result，清掉 warn 噪声）。
- 顺序：任务 1 task_rate（metrics+runtime）→ 任务 2 record_use（runtime+cli）→ 任务 3 文档 →
  任务 4 收尾（fmt/make check/反向验证/提交 feat/memory-lifecycle 不 push）。
- 最大风险：metrics hook 与 diagnose hook 触发顺序在 MaxSteps/Paused 路径上 metrics 先于
  diagnose（agent.rs:2037 vs 2056）→ task_rate 双端接线（metrics 读报告兜底 + diagnose 补写
  评分卡）；record_use 集合只能从 recall 注入侧收集（builder 加可选参数，闭包内 3 行加法）。
- 基线证据（2026-08-05 实测）：feat/memory-lifecycle@68fb094 干净；metrics 17 / runtime 48 /
  agent 231 / cli 32 / workspace 0 failed（2 既有 ignored）。

## 自验收清单（双域，执行者逐条打勾，命令亲跑）
- [x] P1 `cargo test -p deepseeknova-metrics`：≥ 18 条通过（task_rate 新增；实测 20 passed）
- [x] P1 `cargo test -p deepseeknova-agent diagnose`：≥ 基线（diagnose 改动不回退；实测见 make check）
- [x] P2 `cargo test -p deepseeknova-runtime`：≥ 49 条通过（record_use 回填新增；实测 51 passed）
- [x] G1/G2 `cargo test -p deepseeknova-graph`：≥ 35 条通过（Go fixture 新增 ≥3；实测 36 passed）
- [x] G3 `cargo test -p deepseeknova-tools`：≥ 基线（deps_code 提示语更新；实测 deps_code 5 passed 含新增 Go 项目测试）
- [x] `make check` EXIT=0；workspace 0 failed（实测 2026-08-05：对方 fmt 后补跑全绿；55 suite ok / 0 failed / 2 既有 ignored）
- [x] 反向验证：task_rate 断言改坏 → 真红（task_rate_roundtrip_and_compute_defaults 1 failed，metrics/src/lib.rs:921）→ 还原 → 真绿（metrics 20 passed）；Go fixture 断言改坏 → 真红 → 还原 → 真绿
- [x] `cargo fmt --check` 通过（全 workspace 零 diff）；make check EXIT=0 全绿（55 suite ok / 0 failed / 2 既有 ignored）；P 域已提交 feat/memory-lifecycle（不 push，哈希见交付汇报）

## P 域任务状态（Protocol 增强收尾，2026-08-05）
- [x] 任务 1：task_rate 落地。metrics `Scorecard` 增 `first_pass: bool` / `retry_rounds: u32`
  （serde default，旧卡兼容）+ `fill_task_rate(first_pass, retry_rounds)` + 文件级辅助
  `update_scorecard_task_rate(dir, session_id, ...)`（读-改-写，缺失/损坏静默跳过，避免
  runtime 引入 serde_json 依赖）。runtime 双端接线：metrics hook 对 Completed 落盘前填
  `first_pass=true`（成功路径无诊断报告，agent suppress）；`attach_diagnose_hook_with_ingest`
  回调在报告落盘后按 `report.failures` 推导覆写（Paused/unverified 路径 metrics hook 先
  触发、失败详情此时才可知）。测试 +3（metrics：roundtrip+compute 默认、旧卡兼容、
  update 辅助）、+2（runtime：成功 run first_pass=true 且无 diagnose 目录、Paused run
  retry_rounds=failures 条数）。
- [x] 任务 2：record_use 回填。`build_agent_with_role_providers` 增可选参
  `session_skills: Option<Arc<Mutex<Vec<String>>>>`（None = 旧行为；`build_agent` /
  `build_agent_with_task_provider` 透传 None 签名不变）；起点召回注入侧在 P2 修复的
  `injected` 循环内把真实注入技能名去重写入收集器。CLI `build_agent` 创建共享 Arc 同时
  传给 builder（Some）与 `attach_metrics_hook_with_fitness`。fitness hook：record_result
  前补 `record_use`（激活计数，spec §13 #9 闭合）；空集合优雅跳过、移除 warn-once 噪声
  与 TODO。测试 +1（runtime E2E：预置技能 → 注入收集 → fitness.json uses=1/successes=1）。
- [x] 任务 3：文档。GUIDE 协议节补 task_rate 扩展字段与 fitness use+result 一行；
  CHANGELOG Added 条目；BLOCKED 无本域遗留（TUI 面板/计划载体/多模型反思属任务书 §2
  范围外）。
- 跨 crate 协议记录（AGENTS.md §1 触发）：预扫描=不动 core 公开 API（SkillManager /
  FitnessStore 零改动）、不改既有测试断言（runtime 4 处 builder 调用仅补 None 实参）、
  runtime memory 装配区 480-635 仅加 3 行收集（不动既有逻辑）；备选路径 A=改
  `build_agent_with_task_provider`/`build_agent` 签名（破坏 20+ 测试调用点）vs B=仅扩
  `build_agent_with_role_providers` 可选参（CLI 直调，其余入口 None 透传）——选 B；
  备选路径 C=runtime 直接读诊断文件推导 task_rate vs D=诊断回调回填评分卡（C 在
  Paused 路径 metrics hook 先于诊断文件落盘、时序不可行）——选 D + metrics hook 对
  Completed 填首过；自检=metrics/runtime/cli 聚焦测试 + make check + 反向验证红→绿。
- [x] 任务 4：收尾（2026-08-05 接手完成）。接手时任务 1-3 代码在**工作树未提交**
  （PROGRESS 此前"已提交"记录失实，已修正）；逐项复核：metrics 20 / runtime 51 /
  cli 32 / agent 231 全绿，cargo fmt --check 零 diff，make check EXIT=0（55 suite
  ok / 0 failed / 2 既有 ignored，graph 曾因对方 worker 瞬时半成品 FAILED 1 次，
  重试即恢复）；反向验证 task_rate 断言改坏 → 真红（1 failed）→ 还原 → 真绿；
  已提交 feat/memory-lifecycle（含任务书 plan 文件，不 push）。

## 任务书：记忆生命周期闭环（2026-08-05，dev-loop 轮次）— 开工回执
- 理解的目标：记忆从"写入→关键词检索"升级为完整生命周期闭环——检索排序融合生命周期
  信号（importance/stage/recency）、衰减接线（apply_decay 死代码复活）、归档超期清理、
  蒸馏双轨统一、schema 版本机制。
- 顺序：任务 0 基线核验 → 1 schema 版本 + archived 检索过滤 → 2 排序融合 →
  3 衰减+cleanup → 4 蒸馏入口统一 → 5 文档/反向验证/提交。
- 最大风险：store.search 是 hot path，排序 SQL 改动不得破坏既有召回；衰减/清理接口
  签名变更涉及 core 公开 API（AGENTS.md §1.1 触发）；weight=0 必须与旧行为等价。
- 基线证据（2026-08-05 实测）：main@8c7c450 工作区干净；cargo test --workspace
  ≈998 通过 / 0 failed / 2 ignored（graph self_index、provider reasoning protocol，
  均在域外）；core 118（memory 61）、agent 236、runtime 48、tools 84、cli 32、
  config 32+18。

## 自验收清单（执行者逐条打勾，命令亲跑）
- [x] `cargo test --workspace --no-fail-fast`：通过数 ≥ 1018 / 0 failed（实测 make check 内 1018 通过 / 0 failed / 2 既有 ignored）
- [x] `cargo test -p deepseeknova-core memory`：≥ 74 条通过（实测 memory:: 74 条；core 全量 132 lib + 2 集成 + 1 doctest）
- [x] `make check` EXIT=0（fmt + clippy + 全 workspace 测试 + doc 全绿）
- [x] CLI 冒烟：`memory stats` 输出含 stage 分布（实测 `stages=archived:1,candidate:1,verified:1 archived=1`）
- [x] CLI 冒烟：`memory cleanup` 空库与有数据均不 panic、输出报告（实测 decayed=3 deleted=1 / 空库 decayed=0 deleted=0）
- [x] 反向验证：改坏排序融合断言 → 真红（1 failed：生命周期融合必须重排）→ 还原 → 真绿（1 passed）+ FMT_OK
- [x] `cargo fmt --check` 通过；提交到分支 feat/memory-lifecycle（不 push）

## 任务状态（记忆生命周期闭环 2026-08-05）
- **review-fix 轮（2026-08-05，ddfd4b4 之上，6/6 全修）**：C1 store 事务化批量衰减 `decay_all`（单事务读-算-写，防并发 record_recall 丢失更新）；C2 蒸馏去重双前缀检查（distill-/reflect- 任一命中即已存在）；C3 runtime 起点/mid-run 召回 + tools recall 工具全部接 `rank_lifecycle_weight`（新增 `MemoryRankWeight` 扩展，缺失回落 0.3 默认）；C4 未来版本库保持原版本号不回写（收紧测试断言）；C5 PROGRESS 措辞修正；C6 decay_rate 入口 clamp(0,1) + 测试。workspace 1016 → 1018 passed / 0 failed。详见 crates/REVIEW.md 修复轮分节。
- **G/P 域 OCR review-fix 轮（2026-08-05，95f695e 之上，4/4 全修）**：G-M1 parser 分组 Go type 声明遍历全部 type_spec 逐个产出实体（+fixture 测试）；G-L4 go.mod `require (` 尾注释容忍 + replace/exclude 负例测试；P-L2 诊断回填仅失败型会话覆写（Cancelled 零失败不被标 first_pass=true）；P-L4 损坏 scorecard parse 失败 warn 一次后 Ok、真实 IO 错误仍 Err（+目录路径传播测试）。graph 38→40、metrics 20→21、runtime 51→52；make check 全绿；反向验证 G-M1/P-L2 新断言红→绿。详见 crates/REVIEW.md 修复轮分节。
- [x] 任务 1：schema 版本机制（`meta.schema_version` 初值 "1"，open 读/写，版本不符走空迁移表不炸，graph 先例同款）+ 核验 archived 排除（原 search **未**排除 → 已补：三路检索 LEFT JOIN memory_meta 过滤 stage='archived'，search_hybrid 嵌入扫描同步排除）。测试 +3（版本写入/旧版本不炸/未来版本不炸）+2（FTS 与 LIKE 路径 archived 不召回）。
- [x] 任务 2：检索排序融合生命周期因子（SQL 内：`bm25 + rank_weight * ((1-importance)*stage_mult - recency_discount)`，stage_mult permanent=1.2/verified=1.1/candidate=1.0，recency 7 天内 0.5/30 天内 0.25）。入口：store.search_with_weight + engine.recall_with_weight（CLI 传 `[memory] rank_lifecycle_weight`；默认入口 recall/search 用 0.3 = 配置默认——**签名不变、行为变**：既有调用方排序从纯 bm25 变为 0.3 融合，weight=0 才等价旧行为）。测试 +1：同文本双条目 weight=0 保持纯 bm25（分数相等=生命周期项关闭）vs weight=0.3 重排（组合回归锚点）。
- [x] 任务 3：衰减接线（engine.decay：非 permanent 衰减、<0.1→archived、permanent 豁免；engine.cleanup：decay + 删除 archived 且距最后召回 > archive_ttl_days，返回 (decayed, deleted)）；store 增 all_lifecycle/update_lifecycle/delete_archived_older_than/stage_counts；MemoryConfig 增 decay_rate=0.1/archive_ttl_days=30/rank_lifecycle_weight=0.3（含分层 merge + 测试）；CLI 增 `memory cleanup`、`memory stats` 输出 stage 分布 + archived 计数。测试 +4（衰减计数+permanent 豁免/超阈值归档/cleanup 只删超期/stats 分布）。
- [x] 任务 4：蒸馏入口统一（engine.record_knowledge(kind,title,body,tags,source)：title 空则省略 title 行，id 前缀统一 "distill"，已存在则跳过写入保留首次 tags/source；record_reflection_lesson/record_llm_knowledge 变薄封装，签名不变）。测试 +1（reflect 写入后 llm-distill 同内容去重、两入口同格式）。
- [x] 任务 5 前半：GUIDE 记忆节补 cleanup/衰减/排序权重说明 + `[memory]` 新字段；CHANGELOG Added 条目。
- [x] 收尾：cargo fmt + make check EXIT=0（1016 通过 / 0 failed / 2 既有 ignored）；反向验证红（1 failed）→ 绿（1 passed）；CLI 冒烟 stats/cleanup 空库+有数据；分支 feat/memory-lifecycle 提交（不 push）。
- 测试计数变化：core lib 118→130（memory 60→72：engine 13→18、store 15→22、lifecycle 7、skill 9、embedding 3、redact 6、recall 2、profile 5；基线书"memory 61/skill 10"实测为 60/9，未动 skill.rs）；config 32→33；cli 32 不变；workspace 998→1016。
- 跨 crate 协议记录（AGENTS.md §1 触发）：预扫描=engine.recall/store.search/open 签名均不可变（runtime/tools 白名单外调用方），权重改走新增方法 `search_with_weight`/`recall_with_weight`（旧签名不变；默认行为从纯 bm25 变为 0.3 融合，属有意变更，0 才等价旧行为）；备选路径 A=store 内部可变字段存权重（需 interior mutability）vs B=新方法传参（选 B，零状态、weight=0 等价可直测）；自检=core/config/cli 聚焦测试 + make check 全绿 + 反向验证红→绿 + CLI 冒烟。
- 遗留→已修（2026-08-05 review-fix 轮）：runtime 侧召回未接 rank_lifecycle_weight（起点召回 + mid-run 召回 + tools recall 工具已全部接线，见 crates/REVIEW.md 本轮 C3 处置）。

## 已提交并推送（2026-08-05 审查修复轮）
- 审查修复（main@b312715 之上，6 项 review finding 全部修复）：
  ① `Config::merge` 补 `quality`/`protocol`/`delegate`/`attribution` 分层合并
  （各段配置真正生效，attribution 不再被项目层缺省覆盖）；
  ② coordinator `depends_on` 目标节点后置时依赖边不再被丢弃（先加节点再补边）；
  ③ serve 每次 run 生成唯一会话标注（未标注时 `session-<ms>-<seq>`），评分卡/
  诊断/Paused 共用同一 id，不再互相覆盖；
  ④ DiagnoseGuard/对抗审查按 run 起点差分切片，findings 不跨 run 污染；
  ⑤ 蒸馏 skill 中文标题可落盘（Unicode slug，拒绝缩成 `.`/`..` 的标题）；
  ⑥ Parallel 失败子节点写回共享容器，Observe 可见失败产出。
  验证：cargo fmt/clippy/check 全绿；cargo test --workspace = 1003 通过 / 0 失败
  / 2 ignored（含 serve 集成与 tools 本地 HTTP 测试）；cargo deny --all-features
  check 通过（unsound 公告已纳入评估）。
  已提交并推送：`5d009f4`（origin/main）。
- 08-04/08-05 迭代（DAG 接线、失败归因重试、技能热更新、TaskSpec、SessionMetrics、
  任务质量闭环、协议增强能力包、安全审查修复）已合入 main：ca814c1 / 2bb9909 /
  b312715。
- 设计/计划文档已存档：`docs/superpowers/specs/2026-08-05-task-quality-loop-design.md`
  （§12 为实现偏差记录）、`docs/superpowers/plans/2026-08-05-task-quality-loop-plan.md`。

## TUI v2 全面重设计 — 状态（2026-08-04 更新）
- 状态：已合入 main（`5cfcd19`）；spec 与 plan 见
  `docs/superpowers/specs/2026-08-03-tui-v2-design.md`、
  `docs/superpowers/plans/2026-08-03-tui-v2.md`。
- 证据：`cargo test -p deepseeknova-tui` = 89 通过；`make check` EXIT=0
  （当前全量 800 通过 / 0 失败 / 2 既有 ignored）；desktop 已随 `3ab55d7`
  移除，相关前端/desktop 条目废止。

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

## 顺手活清仓（除前端）— 开工回执（2026-08-02）
- 目标：BLOCKED 里除前端外的顺手活全部落地：README 数字、TUI 成本/diff 高亮/MCP 探测/多行输入、verify LLM 化。
- 顺序：README → TUI 成本+diff → TUI MCP 探测 → TUI 多行输入 → verify LLM → 文档/反向验证/PR。
- 最大风险：多行输入改动 draw/布局/键位，既有 TUI 测试必须保绿；verify LLM 跨 config+agent+runtime，按 AGENTS.md §1 记录（预扫描=不动机器契约/既有断言，备选=LLM 失败优雅跳过 vs 硬阻断，选跳过与 review 一致；自检=单 crate 测试+make check+反向验证）。
- 基线证据（本轮实测）：main@2c71284；make check EXIT=0；tui 35+1 绿 0 ignored；agent 119 绿；runtime 26 绿。

## 任务状态（顺手活清仓）
- [x] README：徽章 536→670（全 workspace 实测）、Tauri 命令 44→63（rg 实测计数）三处。
- [x] TUI 状态栏常驻成本：AppState.total_cost_usd 每帧从 router ledger 刷新；status_segments 增 cost 段（$0.000000 精度）；/cost 明细保留。
- [x] TUI diff 高亮：diff_spans 纯函数（+ 绿 / - 红 / @@ 青 / +++--- 不改色），应用于 ToolResult/Agent 行。
- [x] TUI /mcp 实时探测：McpServerInfo + McpStatus + McpProbe trait；CLI 实现 CliMcpProbe（短超时 spawn，stdin 保持打开防假阴性，存活=已连接）；/mcp 显示 ✓/✗。
- [x] TUI 多行输入：Shift+Enter / Ctrl+J 换行；行内 Home/End；↑/↓ 多行移光标（单行仍走历史）；input_view 纵向跟随 + 横向窗口；输入框 3→5 行；提示行/帮助/文档同步。
- [x] verify LLM 化：VerifyConfig 增 llm/llm_model/llm_max_chars（默认关）；Agent::with_llm_verify；verify.rs 增 render/parse/run_llm_verify_pass（JSON 契约 {"passed": bool, "reason": ...}，失败才回炉，调用/解析失败 Skipped）；runtime 装配 provider（回落 main）。
- [x] 收尾：cargo fmt + make check EXIT=0（全 workspace 684 通过）；反向验证三连红→绿（diff 色、多行光标、LLM 判定）；README 徽章 684。

## 交付
- 分支 feat/side-tasks @ 2f79b72，已推送 origin；PR: https://github.com/W117C/DeepseekNova/pull/56
## 任务书：代码库智能（References + 依赖图 + deps_code）— 开工回执（2026-08-02）
- 理解的目标：graph crate 真实生成 References 边（谁引用符号）、结构化依赖图（import/use/require + 清单依赖 + 文件间边）、新工具 deps_code；runtime 提示词补一行；文档同步；PR。
- 顺序：任务 0 → 1 References（parser+store+schema v4+单测）→ 2 依赖图（import 事实+清单解析+重建）→ 3 deps_code 工具 → 4 文档/反向验证/PR。
- 最大风险：引用采集误伤（call callee 已走 Calls，靠「已有 Calls 边不重复加 References」去重）；schema v4 迁移强制全量重解析；清单行级解析可能漏花式语法（BLOCKED 已记）。
- 基线证据（本轮实测）：main@2c71284；make check EXIT=0、670 通过；graph 24+1；tools 59+12+7（书里 50+12+7 为 PR #55 合入前数字，≥ 硬指标）；runtime 26。
- 工作树说明：存在无关未跟踪文件 codex_desktop_ui.html（非本书产物，白名单外，不动）。

## 任务状态（代码库智能）
- [x] 任务 1：References 边（parser 定义体引用采集：去重/上限 64/跳过 callee 与自身名；raw_refs 表；schema v4；全局重建按名匹配、Calls 已覆盖不重复）；graph 新增 3 条单测（同文件/跨文件/递归不自引用）。
- [x] 任务 2：结构化依赖图（Rust use 段/Python import 段/JS specifier 本地 vs 外部 + require；raw_import_links + raw_external_deps；清单解析 Cargo/package.json/pyproject 行级；重建：符号按名 文件→符号、相对路径 文件→文件、外部进表）；graph 新增 3 条单测（serde 外部依赖/JS 文件边/Python 按名命中）。
- [x] 任务 3：deps_code 工具（entity 可选 + direction + external；依赖/依赖方/外部 [external]/无 entity 全库汇总；加入 graph_query_tools()）；GRAPH_RETRIEVAL_HINT 补第 7 条；tools 新增 3 条测试。
- [x] 任务 4 前半：GUIDE（A3.1 补 deps_code + 新增 A3.2 节）、graph README、CHANGELOG 已更新。
- [x] 收尾：cargo fmt + make check EXIT=0（681 通过）；反向验证红（1 failed）→还原绿（graph 32+1、tools 62+12+7）；分支 feat/code-intel 已推送。

## 交付
- 分支 feat/code-intel @ c386c1a，已推送 origin；PR: https://github.com/W117C/DeepseekNova/pull/57
## 任务书：长期记忆 LLM 蒸馏 — 开工回执（2026-08-02）
- 理解的目标：`[memory] llm_distill`（默认 false）：回合结束 DistillHook 先跑启发式 record_task 兜底，启用时另 spawn 异步 LLM 蒸馏（JSON 契约 skill/lesson）→ core Skill 类目落库（去重+脱敏）。
- 顺序：任务 0 → 1 core record_llm_knowledge → 2 agent memory_distill 模块 → 3 config+runtime 装配 → 4 文档/反向验证/PR。
- 最大风险：DistillHook 是同步签名，需 tokio Handle spawn；main 上 review::extract_json 私有（改可见性不在白名单）→ 模块内自带等价实现；stub 空文本不会 Done（跑满步数 Paused），装配测试按「流正常结束」断言。
- 基线证据（本轮实测）：main@2c71284 干净；core 83+2+1、agent 119+1、config 20+18、runtime 26 全绿 0 ignored；make check 670。

## 任务状态（长期记忆 LLM 蒸馏）
- [x] 任务 1：core MemoryEngine::record_llm_knowledge（Skill 类目、content 哈希去重、redact、tags 追加 llm-distill）+ 2 条单测（存储去重、脱敏）。
- [x] 任务 2：agent memory_distill.rs（render_distill_prompt / parse_distilled / run_llm_distill，自带 extract_json 等价实现）+ 5 条单测（契约、skill/lesson 解析、垃圾→None、provider 失败→None、截断）。
- [x] 任务 3：MemoryConfig 增 llm_distill / llm_distill_model / llm_distill_max_chars（默认 false/None/3000）+ 测试；runtime DistillHook 包装（启发式兜底 + tokio Handle spawn 异步蒸馏，回落 main）；runtime 装配测试 1 条（27 绿）。
- [x] 任务 4 前半：GUIDE 记忆节、CHANGELOG 已更新。
- [x] 收尾：cargo fmt + make check EXIT=0（678 通过）；反向验证红（1 failed）→还原绿（core 85、agent 124、runtime 27）；分支 feat/memory-distill 已推送。

## 交付
- 分支 feat/memory-distill，已推送 origin；PR: https://github.com/W117C/DeepseekNova/pull/58
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

## 任务书：记忆语义检索（embedder）最小闭环（2026-08-05，dev-loop 轮）— 开工回执
- 理解的目标：把记忆召回从关键词命中升级为语义相关——remote OpenAI 兼容嵌入后端
  （零新增依赖）、写入即嵌入、旧记忆显式回填、hybrid 检索（bm25+余弦+生命周期权重），
  runtime/CLI 装配 + 文档 + 审查收尾。
- 顺序：任务 0 基线核验 → 1 store hybrid+weight → 2 provider RemoteEmbedder →
  3 config → 4 engine 接线 → 5 runtime/CLI 装配 → 6 文档/反向验证/收尾。
- 最大风险：hybrid 与生命周期权重双重计权（需 FTS 基数改纯 bm25）；同步 trait 内
  做 HTTP 调用（RemoteEmbedder 持独立 tokio runtime block_on，不阻塞调用方 runtime）；
  config merge 默认值语义；既有 52 条 runtime 测试与 35 条 provider 测试不许回退。

## 自验收清单（执行者逐条打勾，命令亲跑）
- [x] A1 `make check` EXIT=0；workspace 0 failed（2 既有 ignored）
- [x] A2 `cargo test -p deepseeknova-core memory::` 全绿且 core 测试数 ≥ 138
      （实测 137 单测 + 2 集成 = 139；memory:: 79 全绿）
- [x] A3 `cargo test -p deepseeknova-provider` 全绿且 ≥ 39（实测 40）
- [x] A4 `cargo test -p deepseeknova-config` ≥ 53（实测 35+18=53）；
      `cargo test -p deepseeknova-runtime` ≥ 53（实测 53）
- [x] A5 `cargo test -p deepseeknova-tools` ≥ 67（实测 67+12+7；memory 工具
      语义命中测试新增）
- [x] B1 temp 目录 CLI 冒烟：`memory stats` 输出含 `embedded=`；`memory
      embed-backfill` 无 provider 不 panic 且 attempted=0（实测见下）
- [x] B2 反向验证：改坏「语义独有命中」断言 → 真红（1 failed）→ 还原 → 真绿

## 任务书：观测台前端 UI + TUI 演进（2026-08-07 dev-loop 轮）— 开工回执
- 理解的目标：把已定稿观测台设计规范落地——桌面端重建纯前端工程（Vite+Solid+TS+Tailwind4）实现 A×B 首屏；TUI 按 §三 演进五项（浅色档 token、夜次分组+星点、审批风险标签+mono 命令、测光评分卡+/scorecard、欢迎卡圆顶字形），保留键位与命令；P0 未提交代码仅作基线提交。
- 顺序：0 基线（提交 P0 + 记录 make check）→ 1 desktop scaffold + vitest + build + 截图 → 2 TUI 浅色档 → 3 夜次分组 → 4 审批风险 → 5 测光评分卡 → 6 欢迎卡 → 7 文档 → 8 全量验收 + 反向验证 → 9 OCR 审查修复 → 10 收尾。
- 最大风险：desktop npm 安装/构建网络与版本（止损：3 次失败或 15 分钟 → 交付源码 + BLOCKED）；审批风险标签跨 permission/agent/tui（additive API，不改 core trait）；全量 make check 耗时长且当前树含 P0 未提交改动（先基线提交）。
- 基线证据（本轮实测）：任务书 `docs/superpowers/plans/2026-08-07-frontend-tui-plan.md`；上一轮 CLOSEOUT 已记录 P0（会话级 HTTP API + serve 认证）未提交工作区 `make check` EXIT=0；本轮基线 `make check` 结果待本轮任务 0 回填。

## 自验收清单（2026-08-07 前端+TUI 轮，执行者逐条打勾，命令亲跑）
- [x] A1 提交完成：`fdefbd9`（含上一轮 P0 + 设计资产 + 本轮改动；排除
      `repro_tmp.rs` 用户调试文件）；未 push
- [x] A2 最终 `make check` EXIT=0（fmt / clippy -D warnings / 全 workspace 测试 /
      doctest / cargo doc 零警告）；TUI 154 单测 + 1 doctest（≥ 基线 + 新增）
- [x] A3 desktop：`cd crates/deepseeknova-desktop/frontend && npm run build` EXIT=0；
      `npx vitest run` 全绿（14 条，≥4）
- [x] A4 截图存在：`.impeccable/mocks/obs-comp-d-desktop-p1.png`（1536×1024，
      Agnes 视觉核对：深色首屏正常、双带/侧栏/对话流就位）
- [x] A5 TUI 聚焦测试：`cargo test -p deepseeknova-tui` 全绿（154 单测 + 1 doctest）；
      `cargo test -p deepseeknova-permission` 35 绿；`cargo test -p deepseeknova-agent` 全绿
- [x] A6 反向验证：改坏夜次分组断言 → 真红（1 failed）→ 还原 → 真绿
- [x] A7 反向验证：改坏 frontend nightKeyFromId → vitest 真红（1 failed）→ 还原 → 真绿
- [x] A5b 聚焦 clippy：`cargo clippy -p permission -p agent -p tui --all-targets -- -D warnings` EXIT=0；
      `cargo fmt --all -- --check` EXIT=0
- [x] A8 OCR：`ocr delegate preview` + `ocr delegate rule` 输出与评论表已贴
      crates/REVIEW.md；R1 high 已修；修复后全量 `make check` EXIT=0
- [x] A9 收尾：CLOSEOUT 六事实面 + BLOCKED 更新；无未确认删除

## CI 修复轮 + 会话级命中率（2026-08-31，rustfmt/clippy 1.98 对齐）

- **工具链对齐**：本机无 Rust → 装 rustup stable（cargo 1.98.0 + rustfmt +
  clippy）；CI 用 dtolnay/rust-toolchain@stable 不锁版本，1.98 的 rustfmt
  格式化行为变化 → 全仓 cargo fmt 独立 style 提交；clippy 1.98 新 lint
  chunks_exact_to_as_chunks 命中 core/memory 与 graph/store 共 3 处 →
  as_chunks::<4>().0 修复；provider 测试 ProviderConfig 初始化缺
  cache_ttl/cache_prompt_key/cache_exact 三字段（HEAD 上已存在的测试
  编译破损）→ 补 None。基线 make check EXIT=0。
- **会话级 prefix cache 命中率**（README 唯一 [规划中] 项落地）：TUI
  状态栏 ⌁N% 段——AppState 新增 session_cache_hit/miss（RunEvent::Usage
  逐调用饱和累计，含子代理/plan mode），三档着色（<30% 黄对齐 runtime
  30% 告警阈值 / ≥70% 绿 / 其余 dim），无可评估数据不显示；复位语义
  /new /resume 清零、/clear 保留。4 条单测 + README/README_EN/CHANGELOG
  同步。设计依据：TUI usage 原为单事件覆盖（仅显示最后一次调用），
  评分卡 cache_hit_rate 只有会话末快照，无实时跨轮统计。
- **CI 红项修复**（dependabot PR 与 main 共同暴露）：
  - RUSTSEC-2026-0258（h2 0.4.15 unbounded empty DATA frames）→
    cargo update -p h2 → 0.4.19（audit/deny 双拦截解除）
  - make bench-ci 双重根因：criterion 0.8 升级后 workspace 配置缺
    cargo_bench_support feature（4c6929d 引入 default-features=false）
    + --workspace 转发参数给 libtest harness（agent --lib 报
    Unrecognized option）→ 补 feature + 配方定向传参 4 个
    harness=false 目标；本地 make bench-ci EXIT=0 实证
  - README 测试数 1926 → 2112（CI 权威口径 2108 + 本轮 4 条新测试）
- **dependabot**：#76 unicode-width / #77 tree-sitter-javascript 全绿
  squash 合并；#74 tokio / #75 uuid / #78 serde_json / #83 install-action
  rebase 后仅剩上述 main 级红项，修复推送后待 rebase 复跑。
- **验证**：make check EXIT=0（fmt / clippy -D warnings / 全 workspace
  测试 / doctest / doc 零警告）+ make bench-ci EXIT=0（本地 macOS
  arm64，criterion 基线 target/criterion baseline=ci）。
