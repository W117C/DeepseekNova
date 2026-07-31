# deepseeknova User Guide

## Table of Contents

1. [Concepts](#concepts)
2. [Installation & Setup](#installation--setup)
3. [Configuration](#configuration)
4. [Tools Reference](#tools-reference)
5. [Security Scan (CLI)](#security-scan-cli)
6. [Skills](#skills)
7. [HTTP API](#http-api)
8. [TUI](#tui)
9. [MCP Integration](#mcp-integration)
10. [Plan Mode](#plan-mode)
11. [Sub-Agents](#sub-agents)
12. [Sandbox](#sandbox)
13. [Advanced Configuration](#advanced-configuration)

## Concepts

deepseeknova is built around a few core abstractions:

### Runner

The `Runner` trait is the central execution abstraction. Everything that can process a prompt and
return results implements `Runner`: the main `Agent`, the `Planner`, `CoordinatorRunner`, and
`SubAgentRunner`.

A `Runner` produces a stream of `RunEvent`s:
- `TextDelta` — streaming text chunks
- `ToolCallStart` / `ToolCallEnd` — tool invocations
- `ToolResult` — tool execution results
- `Usage` — token usage statistics
- `Done` — final output

### Tool

Tools give the agent the ability to interact with the world: read files, run shell commands,
search code, fetch URLs, and more. Every tool implements the `Tool` trait:

```rust
pub trait Tool: Send + Sync {
    fn schema(&self) -> ToolSchema;
    fn read_only(&self) -> bool { false }
    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String>;
}
```

### Registry

The `RegistryHub` holds all named resources: tools, providers, planners, skills, and commands.
Components register themselves and the runtime wires everything together.

### Memory

The agent maintains a conversation history (`Memory`) with automatic compaction.
When the context approaches token limits, older messages are summarized to make room.

## Installation & Setup

### Prerequisites

- Rust 1.75 or later (`rustup update stable`)
- An API key for your LLM provider (OpenAI, Anthropic, or compatible)

### Build from Source

```bash
git clone https://github.com/W117C/DeepseekNova.git
cd deepseeknova-rs
cargo build --release
```

The binary is at `target/release/deepseeknova`.

### Initialize a Project

```bash
deepseeknova init
```

This creates a `.deepseeknova/` directory with:
```
.deepseeknova/
├── config.toml        # Project configuration
├── skills/            # Custom skills (markdown + frontmatter)
├── commands/          # Custom slash commands
└── sessions/          # Session persistence (JSONL)
```

### Setup Wizard

```bash
deepseeknova setup
```

Walks through provider selection, API key configuration, and tool preferences.

## Configuration

Configuration is merged from multiple sources (last wins):

1. **Built-in defaults**
2. **User config**: `~/.config/deepseeknova/config.toml`
3. **Project config**: `.deepseeknova/config.toml`
4. **Environment variables**: `DEEPSEEKNOVA_PROVIDER_MODEL`, `DEEPSEEKNOVA_MAX_STEPS`, etc.

### Full Configuration Reference

```toml
# .deepseeknova/config.toml

[default_provider]
kind = "openai"                    # openai | anthropic | ollama
base_url = "https://api.openai.com/v1"
model = "gpt-4o"
api_key_env = "OPENAI_API_KEY"
max_tokens = 4096
temperature = 0.7

[agent]
max_steps = 25                     # Max tool-calling iterations per turn
system_prompt = "You are a helpful software engineer."
compaction_threshold = 32000       # Tokens before memory compaction
concurrent_tools = true            # 同批读类工具并发、写类保序串行（P1）
# step_effort_routing = true       # 每步在 quick（thinking off）/ high 间切换（P2）
# observe_compress = true          # 超阈值工具输出由廉价模型摘要后入历史（P2）
# observe_compress_threshold_chars = 12000
# observe_compress_max_chars = 4000
# tool_cache = true                # 会话内只读工具结果缓存，写后失效（P2）

[tools]
sandbox = true                     # Enable sandbox for shell commands
allowed_dirs = ["src/", "tests/"]  # Restrict file access
read_only = false                  # Allow write/edit tools

[permissions]
default_policy = "ask"             # ask | allow | deny
auto_allow_tools = ["read_file", "grep", "glob", "ls"]

[mcp]
servers = [
  { name = "filesystem", command = "npx", args = ["-y", "@modelcontextprotocol/server-filesystem", "."] }
]
```

### 模型指针与成本分账

按角色路由模型（均可选；未配置的角色回落 `main`，`main` 未配置则用默认 provider）：

```toml
[model_pointers]
main = "deepseek-v4"          # 主对话
task = "deepseek-v4-flash"    # 子代理/委派
compact = "deepseek-v4-flash" # 历史压缩
quick = "deepseek-v4-flash"   # 快速操作

[[models]]
name = "deepseek-v4"
provider = "deepseek"
input_price_per_mtok = 0.28    # $/1M tokens，可选；配齐 input+output 才输出美元估算
output_price_per_mtok = 0.42
cache_hit_price_per_mtok = 0.028
```

会话内：`/model` 查看指针，`/model use <role> <model>` 热切换（不写盘），`/cost` 查看
按 模型×角色 的 token 用量与成本估算。

coordinator 模式（`run --planner-model ...`）的 Delegate 子代理使用 `task` 指针，
其历史压缩使用 `compact` 指针并按 Compact 角色计量。Agent 的 L3 压缩同样走
`compact` 指针（`agent.compact_model` 仅在指针未设时作为覆盖，照样计量）；
完成前自审门禁（`[review]`）走 `quick` 指针（`review.review_model` 同理作为覆盖）。

### 完成前确定性验证（`[verify]`）

写入类工具执行过后、模型宣布完成之前，按配置命令自动验证（默认关闭）。命令经
`bash` 工具运行，沙箱、命令白名单与资源限制全部生效；失败结果回喂循环让模型修复，
超过 `max_cycles` 后以 `Paused(verify_failed)` 交人工：

```toml
[verify]
enabled = true                        # 默认 false
commands = ["cargo check --quiet"]    # 按序执行，任一失败即回炉
max_cycles = 1                        # 失败回炉上限
```

验证通过后继续原有流程（B3 自审 / Done）。命令需同时满足 `[security]` 的
`allowed_commands`（启用时），未命中白名单会作为验证失败处理。

验证命令的逐条结果通过 `verification` 事件推送给前端（桌面端显示为 `✓ / ✗` 系统行，
HTTP API 为 `event: verification` 的 SSE）。

### 每步 effort 路由与观察压缩（P2）

`step_effort_routing = true` 时，Agent 在每步按规则选择 provider：上一步是正常工具
结果 → `quick` 指针模型（thinking off，省 reasoning token）；首步、工具报错、验证/
审查回炉 → `high` 推理。实现上由 CLI 为同一 main 模型构建两个 effort 实例
（Disabled / High）；运行时未注入这两个实例时回落固定主 provider 并告警。

`observe_compress = true` 时，超过 `observe_compress_threshold_chars` 的工具输出会由
廉价模型（compact 指针优先）压缩为 `observe_compress_max_chars` 字符以内的结构化摘要
再进入历史；前端事件流仍透出原始结果。压缩失败自动回退原有截断行为。

`tool_cache = true` 时，会话内只读工具按（工具名, 参数）缓存结果，同参重复读调用直接
复用（标记 `[cached]`）；任何写工具执行后缓存整体失效。

Coordinator 模式（`run --planner-model ...`）现在同样接入代码图索引：图检索工具
（search_code / traverse_graph / retrieve_entity）对执行器可用，只读工具对规划器开放；
`[graph] enabled = false` 时自动排除。

### Environment Variables

| Variable | Description |
|---|---|
| `OPENAI_API_KEY` | OpenAI API key |
| `ANTHROPIC_API_KEY` | Anthropic API key |
| `DEEPSEEKNOVA_PROVIDER` | Override provider kind |
| `DEEPSEEKNOVA_MODEL` | Override model name |
| `DEEPSEEKNOVA_MAX_STEPS` | Override max steps |
| `DEEPSEEKNOVA_LOG` | Log level (trace, debug, info, warn, error) |

## Tools Reference

### File System

| Tool | Description | Read-only |
|---|---|---|
| `read_file` | Read file contents (max 1MB) | Yes |
| `write_file` | Write/create a file (atomic) | No |
| `edit_file` | SEARCH/REPLACE in a file | No |
| `move_file` | Move or rename a file | No |
| `ls` | List directory contents | Yes |
| `glob` | Find files by glob pattern | Yes |

### Search

| Tool | Description | Read-only |
|---|---|---|
| `grep` | Search file contents with regex | Yes |

### Execution

| Tool | Description | Read-only |
|---|---|---|
| `shell` | Execute a shell command | No |

### Web

| Tool | Description | Read-only |
|---|---|---|
| `web_fetch` | Fetch and parse a URL | Yes |

### Memory

| Tool | Description | Read-only |
|---|---|---|
| `remember` | Store a fact in persistent memory | No |
| `forget` | Remove a fact from memory | No |
| `recall` | Search persistent memory | Yes |

### Task Management

| Tool | Description | Read-only |
|---|---|---|
| `todo_write` | Create/update a structured task list | No |

### Skills

| Tool | Description | Read-only |
|---|---|---|
| `skill__<name>` | Activate a skill (one per registered skill) | Yes |

## Security Scan (CLI)

`deepseeknova scan`（deepsec 式，P1）：内置正则 matcher 零 AI 定位候选点
（硬编码密钥、SQL 拼接、命令注入面、Rust panic 面），再对每个 finding 起
一次性 agent（`task` 指针）调查判真伪，token 计入 Task 角色；命中结果按
severity 分组输出，`--no-ai` 跳过 AI 调查只出 matcher 结果。

| 参数 | 说明 |
|---|---|
| `--path <dir>` | 扫描根目录（默认当前目录）；路径逃逸工作区时 fail-closed 直接报错 |
| `--format md\|json` | 报表格式，默认 `md` |
| `--no-ai` | 跳过 AI 调查，只输出 matcher 命中 |
| `--severity-min high\|medium\|low` | 报告的最低严重级别，默认 `low`（全部） |

示例：

```bash
deepseeknova scan --format json --no-ai
deepseeknova scan --path crates/deepseeknova-cli --severity-min high
```

## Skills

Skills are reusable prompt templates that extend the agent's capabilities. They live in
`.deepseeknova/skills/` as markdown files.

### File Format

```markdown
---
name: my-skill
description: What this skill does
model: claude-sonnet-5     # optional — preferred model
tools_allowed:              # optional — restrict available tools
  - read_file
  - grep
---
# System Prompt

Detailed instructions for how the agent should behave when this skill is active.
```

### Built-in Skills

Place skill files in `.deepseeknova/skills/`. The agent discovers them automatically on startup.
When activated, the skill's system prompt is injected into the conversation.

### Example: Code Reviewer

```markdown
---
name: code-reviewer
description: Review code for bugs, security issues, and style problems
tools_allowed:
  - read_file
  - grep
  - glob
  - ls
---
# Code Reviewer

You are a senior software engineer conducting a code review. For each issue found:

1. **Severity**: CRITICAL | HIGH | MEDIUM | LOW
2. **File & Line**: Where the issue is
3. **Summary**: One-line description
4. **Explanation**: Why it's a problem
5. **Fix**: Concrete suggestion

Check for:
- Logic errors and edge cases
- Security vulnerabilities (OWASP Top 10)
- Performance issues (N+1 queries, missing indexes)
- Missing error handling
- Test coverage gaps
```

## HTTP API

Start the server:

```bash
deepseeknova serve --port 3000 --host 127.0.0.1
```

### Endpoints

#### `GET /health`

Returns server status.

```bash
curl http://localhost:3000/health
# {"status":"ok"}
```

#### `POST /v1/chat`

Streaming chat with Server-Sent Events (SSE).

**Request:**
```json
{
  "prompt": "Explain the Builder pattern in Rust",
  "model": "gpt-4o",
  "images": ["data:image/png;base64,..."]
}
```

**Response (SSE stream):**
```
event: text
data: The Builder pattern...

event: tool_start
data: {"id":"call_1","name":"read_file"}

event: tool_end
data: {"id":"call_1","name":"read_file","arguments":"{\"path\":\"src/lib.rs\"}"}

event: tool_result
data: {"call_id":"call_1","result":"pub struct Builder..."}

event: usage
data: {"prompt_tokens":150,"completion_tokens":200,"total_tokens":350}

event: done
data: {"text":"...","tool_calls":[...],"usage":{...}}
```

### JavaScript Client Example

```javascript
const response = await fetch('http://localhost:3000/v1/chat', {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ prompt: 'Hello!' })
});

const reader = response.body.getReader();
const decoder = new TextDecoder();

while (true) {
  const { done, value } = await reader.read();
  if (done) break;
  const text = decoder.decode(value);
  // Parse SSE events from text
  console.log(text);
}
```

## TUI

Launch the interactive terminal UI:

```bash
deepseeknova chat
```

### Layout

```
┌─ 💬 ready ──────────────────────────────────────┐
│                                                  │
│  User: What files are in src/?                   │
│  ⚙ ls ...                                       │
│    → src/main.rs, src/lib.rs                     │
│  Agent: The src/ directory contains...           │
│                                                  │
├──────────────────────────────────────────────────┤
│ ↑150 ↓200 total:350 | 4 lines                    │
├─ > prompt (Esc to quit) ────────────────────────┤
│ your prompt here...                              │
└──────────────────────────────────────────────────┘
```

### Key Bindings

| Key | Action |
|---|---|
| `Enter` | Submit prompt |
| `Esc` / `q` | Quit (when idle) |
| `Backspace` | Delete last character |

## MCP Integration

deepseeknova can connect to MCP (Model Context Protocol) servers for additional tools.

### Configuration

```toml
[mcp]
servers = [
  { name = "filesystem", command = "npx", args = ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/allowed/dir"] },
  { name = "github", command = "npx", args = ["-y", "@modelcontextprotocol/server-github"] },
]
```

MCP tools are namespaced: `mcp__<server>__<tool>`.

## Plan Mode

Plan mode separates thinking from doing:

1. **Plan**: The agent analyzes the task and produces an `ExecutionGraph` — a DAG of steps with
   dependencies, retry policies, and edge conditions.
2. **Execute**: The graph executor runs steps concurrently where possible, respecting dependencies.

```bash
# Enable plan mode
deepseeknova run --plan "Refactor the auth module to use JWT"
```

### Execution Graph Nodes

| Node Type | Description |
|---|---|
| `Think` | Reasoning step (no side effects) |
| `CallTool` | Run a tool |
| `Observe` | Collect output from a previous step |
| `Reflect` | Evaluate results and decide next actions |
| `Delegate` | Hand off to a sub-agent |
| `Parallel` | Run multiple nodes concurrently |
| `Conditional` | Branch based on a condition |

## Sub-Agents

For complex tasks, the agent can delegate to sub-agents with isolated contexts:

```bash
deepseeknova run "Audit the entire codebase for security issues"
```

The coordinator agent spawns sub-agents for independent work (e.g., one per module) and
synthesizes their results.

## Sandbox

When `tools.sandbox = true`, shell commands run in an OS-level sandbox:

- **macOS**: Seatbelt (Apple Sandbox)
- **Linux**: bubblewrap (bwrap)
- **Windows**: Restricted token (planned)

Read-only tools (`read_file`, `grep`, `glob`, `ls`) are unaffected by sandbox settings.

## Advanced Configuration

### Custom Provider

```toml
[providers.anthropic]
kind = "anthropic"
base_url = "https://api.anthropic.com/v1"
model = "claude-sonnet-5"
api_key_env = "ANTHROPIC_API_KEY"
max_tokens = 8192
```

### Copilot Provider

```toml
[providers.copilot]
kind = "openai"
base_url = "https://api.githubcopilot.com"
model = "gpt-4o"
api_key_env = "GITHUB_TOKEN"
```

### Multiple Providers

```toml
[default_provider]
kind = "openai"
model = "gpt-4o"

[providers.anthropic]
kind = "anthropic"
model = "claude-sonnet-5"

# Use Anthropic for specific skills or plan mode
[plan_mode]
provider = "anthropic"
model = "claude-opus-4-8"
```

### Memory Compaction

When the conversation exceeds `agent.compaction_threshold` tokens, older messages are
automatically summarized:

```toml
[agent]
compaction_threshold = 32000   # Tokens before compaction
pinned_messages = 4            # Keep the N most recent messages unsummarized
```

### Permission Policies

```toml
[permissions]
default_policy = "ask"

# Auto-allow safe tools
auto_allow_tools = ["read_file", "ls", "glob", "grep"]

# Always require confirmation for destructive tools
[[permissions.rules]]
tool = "shell"
policy = "ask"
require_confirmation = true

[[permissions.rules]]
tool = "write_file"
policy = "ask"
allowed_dirs = ["src/", "tests/", "docs/"]

# Deny by path pattern
[[permissions.rules]]
tool = "read_file"
policy = "deny"
path_pattern = "*.env"
```
