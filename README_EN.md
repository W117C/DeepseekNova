<div align="center">

# 🌟 DeepseekNova

### A DeepSeek-Native AI Coding Agent Framework

**22 Rust crates · 3 frontends (CLI / TUI / HTTP API)**

A Rust-from-scratch AI agent framework — not a wrapper. Built specifically for DeepSeek models.

[English](README_EN.md) | [中文](README.md)

</div>

---

<!-- CI Badges -->

<div align="center">

[![CI](https://github.com/W117C/DeepseekNova/actions/workflows/ci.yml/badge.svg)](https://github.com/W117C/DeepseekNova/actions/workflows/ci.yml)
[![Security](https://github.com/W117C/DeepseekNova/actions/workflows/security.yml/badge.svg)](https://github.com/W117C/DeepseekNova/actions/workflows/security.yml)
[![Release](https://github.com/W117C/DeepseekNova/actions/workflows/release.yml/badge.svg)](https://github.com/W117C/DeepseekNova/actions/workflows/release.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![Rust](https://img.shields.io/badge/rust-stable%201.97-orange.svg)](https://www.rust-lang.org)

</div>

---

## 🎯 Key Features

- **Deep reasoning + tool calling** — streaming reasoning output, 4-level reasoning effort, 17 built-in tools
- **Daily experience** — `web_search` (DuckDuckGo / Tavily / Bing / SearXNG),
  `lsp_diagnostics` (auto-injected into tool results after write/edit/move),
  auto model+thinking routing (`[agent] auto_route = true`), and durable
  serve runs (`GET /v1/runs` / `POST /v1/runs/{id}/resume`)
- **Prefix-cache architecture** — cross-turn prompt prefix hits, real-time token tracking, budget control
- **Sandboxed execution** — macOS Seatbelt / Linux bubblewrap isolation;
  allow/ask/deny rule gating (`deny > ask > allow > default mode`) with session
  caching and optional rate limiting; a four-layer read-only shell command
  classifier (arbitrary-args / zero-args / exact form / subcommand flag
  allowlists) skips prompts, while ordinary shell composition (command
  substitution, chaining, redirection) is treated as non-read-only and flows
  through the permission gate (ask/allow rules can approve it); tool-level
  injection surfaces (`git -c`/`--config-env`, format-string injection,
  UNC/URL/SMB path forms) are hard-denied and cannot be overridden by rules
- **Multi-agent delegation** — delegate-based sub-agents (explorer / coder / tester / reviewer) with constrained tool sets, semaphore concurrency, and capped result summaries; isolated context, no recursion. Historical GOAP/Swarm/Federation experiments were removed in B0 (see DESIGN.md).
- **MCP protocol** — stdio + HTTP dual transport, auto-discovery
- **Project knowledge** — Wiki generation, knowledge cards, 4-layer memory distillation, file checkpoints
- **Protocol execution engine (`[protocol]`)** — DNA five-phase gating (Understand→Plan→Execute→Verify→Distill) with built-in gates (plan-before-execute / verify-evidence / distill-on-complex / drift-detection), `hard|soft|off` levels, off by default with zero overhead; evidence-anchored verification (blocking on configured-but-unverified, `unverified` diagnose outcome); adversarial review sub-agent on trigger conditions
- **Skill self-evolution** — usage/success tracking (fitness), deprecate / merge / promote suggestions, deprecated filtering
- **Failure-pattern feedback** — failed sessions clustered into a redacted store, top-3 patterns auto-injected into the next session's first system prompt

## 🏗️ Architecture

```
Frontend    ratatui (TUI) · clap (CLI) · axum HTTP + SSE (API)
               │ WireEvent / SSE / CLI output
Runtime     Agent Loop · Coordinator · Plan-Mode Runner
            Event Bus · Permission Gate · Security Context
               │
Core        Runner Trait · Tool Trait · Registry · WireEvent
               │                    │
Provider    DeepSeek V4 Pro/Flash   Tools: File · Glob · Grep · Shell
            Streaming + Tools       WebFetch · Task · MCP Bridge · 17
```

## 📦 Crates

| Crate | Role |
|-------|------|
| `deepseeknova-core` | Core types: Runner / Tool trait, Registry, WireEvent |
| `deepseeknova-agent` | Agent loop, Coordinator, Plan-Mode Runner |
| `deepseeknova-provider` | DeepSeek / OpenAI-compatible / Anthropic streaming |
| `deepseeknova-tools` | 17 built-in tools + web search + LSP diagnostics + Context7 docs |
| `deepseeknova-mcp` | MCP protocol client (stdio / HTTP) |
| `deepseeknova-metrics` | Session-level effectiveness metrics + JSON reports |
| `deepseeknova-graph` | Code graph engine (tree-sitter + SQLite FTS5 + PageRank + repo map) |
| `deepseeknova-sandbox` | Sandbox trait + macOS Seatbelt / Linux bubblewrap |
| `deepseeknova-permission` | Allow / Ask / Deny permission gate |
| `deepseeknova-security` | Path restrictions, resource limits, audit logging |
| `deepseeknova-scanner` | deepsec-style security scanning: rule matching + optional AI investigation (`scan` subcommand) |
| `deepseeknova-checkpoint` | Filesystem snapshots + transactional rollback |
| `deepseeknova-context` | Workspace indexing, project memory, session state |
| `deepseeknova-skills` | Markdown skill system (.claude/skills compatible) |
| `deepseeknova-store` | JSONL session persistence + rotation + compression |
| `deepseeknova-telemetry` | OpenTelemetry distributed tracing (OTLP) |
| `deepseeknova-event` | Agent lifecycle event bus |
| `deepseeknova-runtime` | Composition root: registry + context + events + permission + security |
| `deepseeknova-config` | Layered TOML config (default → user → project → env → CLI) |
| `deepseeknova-cli` | CLI frontend: chat / plan / scan / serve / setup |
| `deepseeknova-tui` | ratatui terminal UI |
| `deepseeknova-serve` | axum HTTP server + SSE streaming |

## 🚀 Quick Start

```bash
# Build CLI from source
cargo build --release -p deepseeknova-cli
```

### Configuration

Use environment variables for API keys — never hardcode them:

```bash
export DEEPSEEKNOVA_API_KEY="your-api-key"
```

```toml
# ~/.deepseeknova/config.toml
default_model = "deepseek-chat"

[[providers]]
name = "deepseek"
kind = "openai-compatible"
base_url = "https://api.deepseek.com/v1"
api_key_env = "DEEPSEEKNOVA_API_KEY"  # reads from env, not hardcoded
model = "deepseek-chat"
```

> ⚠️ **Windows sandbox**: Shell tool sandboxing is only available on macOS (Seatbelt) and Linux (bubblewrap). Windows uses `NoOpSandbox` (no isolation). Configure `allowed_commands` and path policies carefully on Windows.
> The CLI prints an explicit runtime warning on Windows because no OS-level
> sandbox backend is available there.

## 📊 CI

| Check | Workflow |
|-------|----------|
| cargo check (workspace) | CI |
| cargo clippy (-D warnings) | CI |
| cargo test (Ubuntu / macOS / Windows) | CI |
| cargo llvm-cov | CI |
| release build (3 platforms) | Release |
| cargo audit + cargo deny | Security |

## 🛠️ Tech Stack

| Layer | Technology |
|-------|-----------|
| Language | Rust (stable 1.97) |
| Backend | Rust + SQLite FTS5 + tokio + axum |
| Frontend | TUI (ratatui) · CLI (clap) · HTTP API (axum + SSE) |
| Tracing | OpenTelemetry (OTLP) |
| Tests | 1108 tests · cargo-llvm-cov · 3-platform CI |

## 📄 License

MIT OR Apache-2.0 — see [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).
