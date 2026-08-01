# Changelog

All notable changes to DeepseekNova will be documented in this file.

## [Unreleased]

### ⚠ Breaking

- `agent.on_max_steps` 默认值为 `"pause"`：max_steps 耗尽不再返回错误，而是发出
  `Paused` 事件并优雅结束（CLI 非交互以退出码 3 结束并打印 resume 提示）。
  依赖旧行为的自动化请显式配置 `[agent] on_max_steps = "error"`。
- `edit_file` 语义收紧：SEARCH 须在文件中**唯一**命中（旧版替换首个匹配）；0 或多处命中
  时整次调用失败、不产生半改。多处编辑请用新的 `edits: [{search, replace}, ...]` 数组。

### Added

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

### Changed

- 审核修复：`[memory] mid_run_*` 配置真实生效（含 `mid_run_graph_top_k` 的代码图命中）；
  记忆库主 FTS 表与 trigram 表写入事务化并在打开时对账回填；蒸馏文件关联仅统计本 run
  新增消息；`AgentRoleProviders` 标记 `#[non_exhaustive]`；`upsert_embedding` 补齐
  `created_at` 时间戳。

- 删除实验性 `deepseeknova-orch` crate（GOAP + Swarm，零业务调用）；其唯一有消费者的组件 `ProgressTracker` 已解耦收编至 `deepseeknova-core::progress`。多智能体能力改由 `deepseeknova-agent` 的 delegate/子代理路径提供。CLI dev-dependency、quickstart 示例的 GOAP 段、release 脚本与 README crate 表中的 orch 引用一并清除。
- `compaction_threshold_tokens` 留空时运行时按 `budget.max_total_tokens / 2` 推导，
  让无损的 L1 结果截断默认生效；显式配置与 `[budget] enabled=false` 时行为不变。
- 内置工具 schema 文案精简 41%（7819→4613 字符），降低每次缓存未命中的固定 token 开销。

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
