# deepnova User Guide

## Table of Contents

1. [Concepts](#concepts)
2. [Installation & Setup](#installation--setup)
3. [Configuration](#configuration)
4. [Tools Reference](#tools-reference)
5. [Skills](#skills)
6. [HTTP API](#http-api)
7. [TUI](#tui)
8. [MCP Integration](#mcp-integration)
9. [Plan Mode](#plan-mode)
10. [Sub-Agents](#sub-agents)
11. [Sandbox](#sandbox)
12. [Advanced Configuration](#advanced-configuration)

## Concepts

deepnova is built around a few core abstractions:

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
git clone https://github.com/W117C/DeepNova.git
cd deepnova-rs
cargo build --release
```

The binary is at `target/release/deepnova`.

### Initialize a Project

```bash
deepnova init
```

This creates a `.deepnova/` directory with:
```
.deepnova/
├── config.toml        # Project configuration
├── skills/            # Custom skills (markdown + frontmatter)
├── commands/          # Custom slash commands
└── sessions/          # Session persistence (JSONL)
```

### Setup Wizard

```bash
deepnova setup
```

Walks through provider selection, API key configuration, and tool preferences.

## Configuration

Configuration is merged from multiple sources (last wins):

1. **Built-in defaults**
2. **User config**: `~/.config/deepnova/config.toml`
3. **Project config**: `.deepnova/config.toml`
4. **Environment variables**: `DPRONIX_PROVIDER_MODEL`, `DPRONIX_MAX_STEPS`, etc.

### Full Configuration Reference

```toml
# .deepnova/config.toml

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

### Environment Variables

| Variable | Description |
|---|---|
| `OPENAI_API_KEY` | OpenAI API key |
| `ANTHROPIC_API_KEY` | Anthropic API key |
| `DPRONIX_PROVIDER` | Override provider kind |
| `DPRONIX_MODEL` | Override model name |
| `DPRONIX_MAX_STEPS` | Override max steps |
| `DPRONIX_LOG` | Log level (trace, debug, info, warn, error) |

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

## Skills

Skills are reusable prompt templates that extend the agent's capabilities. They live in
`.deepnova/skills/` as markdown files.

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

Place skill files in `.deepnova/skills/`. The agent discovers them automatically on startup.
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
deepnova serve --port 3000 --host 127.0.0.1
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
deepnova chat
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

deepnova can connect to MCP (Model Context Protocol) servers for additional tools.

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
deepnova run --plan "Refactor the auth module to use JWT"
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
deepnova run "Audit the entire codebase for security issues"
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
