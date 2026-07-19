# Changelog

All notable changes to DeepseekNova will be documented in this file.

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
- `.gitignore` 追加 `.reasonix/`；`README.md` 新增「安全」段与 `deepseeknova-security` crate 条目；`CONTRIBUTING.md` 追加 `deepseeknova-security`；`CHANGELOG.md` 新增 `[0.3.0]`
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
