# reasonix

[![CI](https://github.com/user/reasonix-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/user/reasonix-rs/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE)

**reasonix** is a modular, extensible AI agent framework for Rust. It provides the building blocks for
creating autonomous agents that can use tools, follow multi-step plans, coordinate with sub-agents, and
interact through CLI, TUI, or HTTP interfaces.

## Architecture

```
                         reasonix-cli (binary)
                              │
         ┌────────────────────┼────────────────────┐
         ▼                    ▼                    ▼
  reasonix-runtime    reasonix-agent       reasonix-provider
  (composition root)  (Runner impl)        (Provider trait)
         │                    │                    │
         └────────────────────┼────────────────────┘
                              │
                       reasonix-core (foundation)
           types / graph / runner / tool / registry
                              │
    ┌─────────┬─────────┬─────┼─────┬─────────┬─────────┐
    ▼         ▼         ▼     │     ▼         ▼         ▼
reasonix-  reasonix-  reasonix-│ reasonix-  reasonix-  reasonix-
config     event      permission│ context    tools      mcp
```

### Crates

| Crate | Description |
|---|---|
| `reasonix-core` | Core types: `Runner` trait, `Tool` trait, `ExecutionGraph`, `RegistryHub` |
| `reasonix-agent` | Agent loop with multi-step reasoning, memory compaction, plan mode, sub-agents |
| `reasonix-provider` | LLM provider abstraction: OpenAI, Anthropic, streaming + retry |
| `reasonix-tools` | Built-in tools: fs (read/write/edit/move), grep, glob, ls, shell, web_fetch, todo, memory |
| `reasonix-mcp` | MCP (Model Context Protocol) client — connect to external tool servers |
| `reasonix-config` | TOML-based configuration with multi-layer merging |
| `reasonix-context` | Workspace indexing, project memory, prompt building |
| `reasonix-permission` | Policy-based permission gating for tool execution |
| `reasonix-event` | Event bus for agent lifecycle events |
| `reasonix-runtime` | Composition root — wires all components together |
| `reasonix-sandbox` | Process sandboxing (macOS Seatbelt, Linux bubblewrap) |
| `reasonix-checkpoint` | File checkpoint/rollback for safe tool execution |
| `reasonix-store` | Session persistence (JSONL) |
| `reasonix-tui` | Terminal UI (ratatui) — interactive chat with streaming |
| `reasonix-serve` | HTTP server (axum) — SSE streaming, OpenAI-compatible API |
| `reasonix-skills` | Skill system — load reusable prompts from `.reasonix/skills/` |
| `reasonix-cli` | CLI binary with subcommands: run, chat, serve, setup, init, config |

## Quick Start

### Prerequisites

- Rust 1.75+
- An OpenAI-compatible API key (set `OPENAI_API_KEY` env var)

### Installation

```bash
git clone https://github.com/user/reasonix-rs.git
cd reasonix-rs
cargo build --release
```

### Basic Usage

```bash
# One-shot: ask the agent to do something
cargo run -- run "List all Rust files in src/"

# Interactive terminal UI
cargo run -- chat

# Start HTTP server with SSE streaming
cargo run -- serve --port 3000

# Initialize a new project with .reasonix/ scaffolding
cargo run -- init

# Interactive setup wizard
cargo run -- setup
```

### Configuration

reasonix looks for configuration in these locations (merged in order):
1. `~/.config/reasonix/config.toml` — user defaults
2. `.reasonix/config.toml` — project overrides
3. Environment variables (`REASONIX_*`)

```toml
# .reasonix/config.toml
[default_provider]
kind = "openai"
model = "gpt-4o"
api_key_env = "OPENAI_API_KEY"

[agent]
max_steps = 25
system_prompt = "You are a helpful software engineer."

[tools]
sandbox = true          # Enable sandbox for shell commands
allowed_dirs = ["src/", "tests/"]
```

### HTTP API

```bash
# Start the server
cargo run -- serve --port 3000

# Health check
curl http://localhost:3000/health

# Streaming chat (SSE)
curl -N -X POST http://localhost:3000/v1/chat \
  -H "Content-Type: application/json" \
  -d '{"prompt": "Explain the repository pattern"}'
```

SSE event types: `text`, `reasoning`, `tool_start`, `tool_end`, `tool_result`, `usage`, `done`, `error`.

### Skills

Skills are reusable prompt templates stored in `.reasonix/skills/`:

```markdown
---
name: code-reviewer
description: Review code for bugs and style issues
tools_allowed:
  - read_file
  - grep
  - glob
---
# Code Reviewer

You are a senior software engineer. When reviewing code:
1. Check for correctness first
2. Look for security vulnerabilities
3. Suggest style and maintainability improvements
4. Note any missing tests
```

The agent can activate a skill during conversation, which injects the skill's system prompt.

### TUI

```bash
cargo run -- chat
```

- **Enter** — submit prompt
- **Esc / q** — quit (when idle)
- Streaming text, tool calls, and token usage displayed in real-time

## Development

```bash
# Run all tests
cargo test --all --workspace

# Check compilation
cargo check --all-targets --workspace

# Lint
cargo clippy --all-targets --workspace -- -D warnings

# Format
cargo fmt --all --check

# Build docs
cargo doc --no-deps --workspace --document-private-items
```

### Project Structure

```
.
├── crates/
│   ├── reasonix-core/       # Foundation: types, traits, registry
│   ├── reasonix-agent/      # Agent loop, memory, plan mode
│   ├── reasonix-provider/   # LLM provider abstraction
│   ├── reasonix-tools/      # Built-in tools
│   ├── reasonix-mcp/        # MCP client
│   ├── reasonix-config/     # Configuration
│   ├── reasonix-context/    # Workspace indexing, context
│   ├── reasonix-permission/ # Permission gating
│   ├── reasonix-event/      # Event bus
│   ├── reasonix-runtime/    # Composition root
│   ├── reasonix-sandbox/    # Process sandbox
│   ├── reasonix-checkpoint/ # File checkpoint/rollback
│   ├── reasonix-store/      # Session persistence
│   ├── reasonix-tui/        # Terminal UI
│   ├── reasonix-serve/      # HTTP server
│   ├── reasonix-skills/     # Skill system
│   └── reasonix-cli/        # CLI binary
├── .github/workflows/       # CI/CD
└── GUIDE.md                 # User guide
```

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.
