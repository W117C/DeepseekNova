# Changelog

All notable changes to reasonix-rs will be documented in this file.

## [0.1.0] — 2026-07-03

### Added

#### Foundation (Phase 0)
- `reasonix-core`: Core type system — `Runner` trait, `Tool` trait, `ExecutionGraph`, `RegistryHub`, `Chunk`, `Usage`
- `reasonix-provider`: LLM provider abstraction with OpenAI and Anthropic implementations, streaming support, retry, factory
- `reasonix-agent`: Main agent loop with multi-step reasoning, memory compaction, plan mode runner, sub-agent runner, coordinator runner
- `reasonix-tools`: 13 built-in tools — read_file, write_file, edit_file, move_file, ls, glob, grep, shell, web_fetch, todo_write, remember, forget, recall
- `reasonix-mcp`: MCP client for connecting to external tool servers
- `reasonix-config`: TOML-based config with multi-layer merging (default → user → project → env)
- `reasonix-context`: Workspace indexing, working memory, project memory (REASONIX.md + .reasonix/memory/)
- `reasonix-permission`: Policy-based permission gating for tool execution (allow/ask/deny)
- `reasonix-event`: Event bus for agent lifecycle events
- `reasonix-runtime`: Composition root — wires registry, context, event, permission, and config together
- `reasonix-cli`: CLI binary with subcommands: run, chat, serve, setup, init, config

#### Planning & Execution (Phase 2)
- `reasonix-core::executor`: Graph executor with topological sort and concurrent execution
- `reasonix-core::planner`: SimplePlanner and Planner trait
- `reasonix-agent::plan_mode`: Plan-first execution (read-only planning → user approval → execute)
- `reasonix-agent::sub_agent`: Sub-agent delegation with isolated contexts
- `reasonix-agent::coordinator`: Two-model coordinator (planner + executor)

#### Safety (Phase 3)
- `reasonix-sandbox`: Sandbox trait with platform-specific impls (macOS Seatbelt, Linux bubblewrap)
- `reasonix-checkpoint`: File checkpoint and rollback manager
- `reasonix-store`: Session persistence (JSONL format)

#### Interface (Phase 4)
- `reasonix-tui`: Terminal UI with ratatui — split-pane, streaming, color-coded output
- `reasonix-serve`: HTTP server with axum — SSE streaming, OpenAI-compatible `/v1/chat` endpoint
- `reasonix-skills`: Skill system — load markdown + YAML frontmatter from `.reasonix/skills/`
- `reasonix-telemetry`: OpenTelemetry integration with OTLP/gRPC and stdout exporters

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
