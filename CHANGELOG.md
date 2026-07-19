# Changelog

All notable changes to deepseeknova-rs will be documented in this file.

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

#### Core Engine Fixes (Phase 1)
- True SSE streaming: switched from `response.text()` buffering to `response.bytes_stream()` line-by-line parsing
- Agent tool execution loop: agent now actually runs tools when model calls them, feeds results back into memory
- DeepSeek thinking mode: `reasoning_effort` parameter, `reasoning_content` streamed as `ReasoningDelta`
- Proper reasoning_content passthrough: `Message` struct now has `reasoning_content` field (required by DeepSeek-V4 when tool calls are involved)
- `extra_body` support: `ChatCompletionRequest` can pass DeepSeek-specific params like `{"thinking": {"type": "enabled"}}`
- Ctrl-C cancellation: `CancellationToken` wired to `tokio::signal::ctrl_c()` for graceful agent interruption

#### CLI & TUI (Phase 2)
- 10+ slash commands: `/exit /new /clear /raw /model /skills /mcp /undo /help`
- 3 display modes: normal (all), lite (hide reasoning), raw (chunk types)
- `/new` session restart loop
- Skills listing from `.deepseeknova/skills/` and `.agents/skills/`
- DeepSeek reasoning content displayed in dim ANSI style

#### Desktop App (Phase 3)
- New `deepseeknova-desktop` crate: Tauri 2.x desktop application
- 7 Tauri Commands: `submit_prompt` (Channel streaming), `cancel_run`, `list_skills`, `list_providers`, `get_config`, `get_capabilities`, `health_check`
- React/TypeScript frontend with streaming chat UI, dark theme, skills panel
- System tray with hide/show/quit
- Single-instance lock
- Window close→hide to tray behavior
- Frontend: components extracted (Transcript, MessageCard, Composer)
- Session-level cache hit rate display in status bar

#### Multi-Agent Orchestration (Phase 4)
- New `deepseeknova-orch` crate: GOAP Goal Planner + Swarm Coordinator
- GoalPlanner: A* planner that decomposes goals into action DAGs using DeepSeek-V4 thinking mode
- SwarmCoordinator: Queen-led multi-agent team coordination
- VectorMemory: Cosine similarity search, text→embedding hashing, importance-based compaction, file persistence

#### DeepSeek-V4 Optimizations
- `reasoning_effort` config (low/medium/high/max)
- `thinking_enabled` provider config flag
- `cache_hit_tokens` / `cache_miss_tokens` parsed from API responses with DeepSeek field names
- `ResponseUsage` updated with `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens` serde aliases

#### Snippet System (Phase 5)
- New snippet module: ReadFileTool returns `[SNIPPED ID: xxx]` for edit validation
- EditFileTool accepts optional `snippet_id`, validates file hasn't changed since read
- Stale edits return current content instead of hard failure
- SHA256 content hashing, global singleton tracker

#### Release Pipeline
- Cargo-dist configuration with multi-target builds
- GitHub Actions release workflow (CLI + Desktop)
- npm installer config

### Fixed
- `blocking_send` in tokio async context → all uses `tx.send().await`
- UTF-8 corruption across TCP chunk boundaries → raw `Vec<u8>` accumulation
- Tool error messages leaking file paths → 500-char truncation with `floor_char_boundary()`
- `&s[..max]` UTF-8 slice panics → `floor_char_boundary()` everywhere
- `[redacted]` placeholder corruption across 29+ source files
- TypeScript `[redacted]` → `number` type fixes in types.ts

## [0.1.0] — 2026-07-03

### Added

#### Foundation (Phase 0)
- `deepseeknova-core`: Core type system — `Runner` trait, `Tool` trait, `ExecutionGraph`, `RegistryHub`, `Chunk`, `Usage`
- `deepseeknova-provider`: LLM provider abstraction with OpenAI and Anthropic implementations, streaming support, retry, factory
- `deepseeknova-agent`: Main agent loop with multi-step reasoning, memory compaction, plan mode runner, sub-agent runner, coordinator runner
- `deepseeknova-tools`: 13 built-in tools — read_file, write_file, edit_file, move_file, ls, glob, grep, shell, web_fetch, todo_write, remember, forget, recall
- `deepseeknova-mcp`: MCP client for connecting to external tool servers
- `deepseeknova-config`: TOML-based config with multi-layer merging (default → user → project → env)
- `deepseeknova-context`: Workspace indexing, working memory, project memory (DPRONIX.md + .deepseeknova/memory/)
- `deepseeknova-permission`: Policy-based permission gating for tool execution (allow/ask/deny)
- `deepseeknova-event`: Event bus for agent lifecycle events
- `deepseeknova-runtime`: Composition root — wires registry, context, event, permission, and config together
- `deepseeknova-cli`: CLI binary with subcommands: run, chat, serve, setup, init, config

#### Planning & Execution (Phase 2)
- `deepseeknova-core::executor`: Graph executor with topological sort and concurrent execution
- `deepseeknova-core::planner`: SimplePlanner and Planner trait
- `deepseeknova-agent::plan_mode`: Plan-first execution (read-only planning → user approval → execute)
- `deepseeknova-agent::sub_agent`: Sub-agent delegation with isolated contexts
- `deepseeknova-agent::coordinator`: Two-model coordinator (planner + executor)

#### Safety (Phase 3)
- `deepseeknova-sandbox`: Sandbox trait with platform-specific impls (macOS Seatbelt, Linux bubblewrap)
- `deepseeknova-checkpoint`: File checkpoint and rollback manager
- `deepseeknova-store`: Session persistence (JSONL format)

#### Interface (Phase 4)
- `deepseeknova-tui`: Terminal UI with ratatui — split-pane, streaming, color-coded output
- `deepseeknova-serve`: HTTP server with axum — SSE streaming, OpenAI-compatible `/v1/chat` endpoint
- `deepseeknova-skills`: Skill system — load markdown + YAML frontmatter from `.deepseeknova/skills/`
- `deepseeknova-telemetry`: OpenTelemetry integration with OTLP/gRPC and stdout exporters

#### Tooling
- CI/CD: GitHub Actions with 6 jobs (check, clippy, fmt, test matrix, docs, release)
- Release profile: `opt-level="s"`, LTO, strip, single codegen unit
- Cross-platform builds: x86_64 Linux, aarch64 macOS, x86_64 Windows

### Testing
- 232 tests (180 unit + 52 integration) across all crates
- Integration tests: Agent loop, tool chains (TempDir), HTTP serve (live server), skills E2E, config merge, memory lifecycle, Runner contracts
- 0 clippy warnings, clean compilation

### Documentation
- Crate-level docs with examples for all public modules
- README with architecture diagram, quick start, and development guide
- GUIDE.md — full user guide covering configuration, tools, skills, API, TUI, MCP, plan mode, sandbox
