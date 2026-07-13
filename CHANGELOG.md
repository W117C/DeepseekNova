# Changelog

All notable changes to dpronix-rs will be documented in this file.

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
- Skills listing from `.reasonix/skills/` and `.agents/skills/`
- DeepSeek reasoning content displayed in dim ANSI style

#### Desktop App (Phase 3)
- New `dpronix-desktop` crate: Tauri 2.x desktop application
- 7 Tauri Commands: `submit_prompt` (Channel streaming), `cancel_run`, `list_skills`, `list_providers`, `get_config`, `get_capabilities`, `health_check`
- React/TypeScript frontend with streaming chat UI, dark theme, skills panel
- System tray with hide/show/quit
- Single-instance lock
- Window close→hide to tray behavior
- Frontend: components extracted (Transcript, MessageCard, Composer)
- Session-level cache hit rate display in status bar

#### Multi-Agent Orchestration (Phase 4)
- New `dpronix-orch` crate: GOAP Goal Planner + Swarm Coordinator
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
- `dpronix-core`: Core type system — `Runner` trait, `Tool` trait, `ExecutionGraph`, `RegistryHub`, `Chunk`, `Usage`
- `dpronix-provider`: LLM provider abstraction with OpenAI and Anthropic implementations, streaming support, retry, factory
- `dpronix-agent`: Main agent loop with multi-step reasoning, memory compaction, plan mode runner, sub-agent runner, coordinator runner
- `dpronix-tools`: 13 built-in tools — read_file, write_file, edit_file, move_file, ls, glob, grep, shell, web_fetch, todo_write, remember, forget, recall
- `dpronix-mcp`: MCP client for connecting to external tool servers
- `dpronix-config`: TOML-based config with multi-layer merging (default → user → project → env)
- `dpronix-context`: Workspace indexing, working memory, project memory (REASONIX.md + .reasonix/memory/)
- `dpronix-permission`: Policy-based permission gating for tool execution (allow/ask/deny)
- `dpronix-event`: Event bus for agent lifecycle events
- `dpronix-runtime`: Composition root — wires registry, context, event, permission, and config together
- `dpronix-cli`: CLI binary with subcommands: run, chat, serve, setup, init, config

#### Planning & Execution (Phase 2)
- `dpronix-core::executor`: Graph executor with topological sort and concurrent execution
- `dpronix-core::planner`: SimplePlanner and Planner trait
- `dpronix-agent::plan_mode`: Plan-first execution (read-only planning → user approval → execute)
- `dpronix-agent::sub_agent`: Sub-agent delegation with isolated contexts
- `dpronix-agent::coordinator`: Two-model coordinator (planner + executor)

#### Safety (Phase 3)
- `dpronix-sandbox`: Sandbox trait with platform-specific impls (macOS Seatbelt, Linux bubblewrap)
- `dpronix-checkpoint`: File checkpoint and rollback manager
- `dpronix-store`: Session persistence (JSONL format)

#### Interface (Phase 4)
- `dpronix-tui`: Terminal UI with ratatui — split-pane, streaming, color-coded output
- `dpronix-serve`: HTTP server with axum — SSE streaming, OpenAI-compatible `/v1/chat` endpoint
- `dpronix-skills`: Skill system — load markdown + YAML frontmatter from `.reasonix/skills/`
- `dpronix-telemetry`: OpenTelemetry integration with OTLP/gRPC and stdout exporters

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
