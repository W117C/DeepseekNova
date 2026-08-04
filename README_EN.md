<div align="center">

# 🌟 DeepseekNova

### A DeepSeek-Native AI Coding Agent Framework

**21 Rust crates · 3 frontends (CLI / TUI / HTTP API)**

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

- **Deep reasoning + tool calling** — streaming reasoning output, 4-level reasoning effort, 21 built-in tools
- **Prefix-cache architecture** — cross-turn prompt prefix hits, real-time token tracking, budget control
- **Sandboxed execution** — macOS Seatbelt / Linux bubblewrap isolation, 12 permission rules
- **Multi-agent delegation** — delegate-based sub-agents (explorer / coder / tester / reviewer) with constrained tool sets, semaphore concurrency, and capped result summaries; isolated context, no recursion. Historical GOAP/Swarm/Federation experiments were removed in B0 (see DESIGN.md).
- **MCP protocol** — stdio + HTTP dual transport, auto-discovery
- **Project knowledge** — Wiki generation, knowledge cards, 4-layer memory distillation, file checkpoints

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
            Streaming + Tools       WebFetch · Task · MCP Bridge · 21
```

## 📦 Crates

| Crate | Role |
|-------|------|
| `deepseeknova-core` | Core types: Runner / Tool trait, Registry, WireEvent |
| `deepseeknova-agent` | Agent loop, Coordinator, Plan-Mode Runner |
| `deepseeknova-provider` | DeepSeek / OpenAI-compatible / Anthropic streaming |
| `deepseeknova-tools` | 21 built-in tools |
| `deepseeknova-mcp` | MCP protocol client (stdio / HTTP) |
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
| Tests | 739 tests · cargo-llvm-cov · 3-platform CI |

## 📄 License

MIT OR Apache-2.0 — see [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).
