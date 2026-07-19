# DeepNova — DeepSeek-V4 原生 AI 编码代理框架

[![CI](https://github.com/W117C/DeepNova/actions/workflows/ci.yml/badge.svg)](https://github.com/W117C/DeepNova/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE)

**DeepNova**（原名 deepnova-rs）是一个深度适配 **DeepSeek-V4** 的模块化 AI 编码代理框架，支持思考模式、上下文缓存、工具调用和多 Agent 编排。提供 CLI、TUI、HTTP API 和 **Tauri 桌面端**四种交互方式。

> 从 esengine/DeepSeek-DeepNova、deepcode-cli、Ruflo、ECC 等顶级项目吸取设计，围绕 DeepSeek 的 prefix cache 和 thinking mode 深度优化。

## 系统编译依赖 (Linux)

由于本项目包含 **Tauri 桌面端**，在 Linux (如 Ubuntu) 系统下编译或运行本地测试需要安装相关的系统库：

```bash
sudo apt update
sudo apt install -y \
  pkg-config \
  libglib2.0-dev \
  libgtk-3-dev \
  libwebkit2gtk-4.1-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev
```

## 特性

- 🧠 **DeepSeek-V4 原生优化** — 思考模式（`reasoning_effort`）、上下文缓存（`cache_hit_tokens`）、`reasoning_content` 正确回传
- 🔧 **真正的 SSE 流式传输** — `bytes_stream()` 逐行解析，跨 TCP 分片 UTF-8 安全
- 🛠️ **工具执行循环** — Agent 自动执行工具调用并反馈结果
- 🎯 **Snippet 系统** — read→snippet_id→edit 验证，防止编辑过期文件
- 🐝 **GOAP 规划器** — A* 目标导向规划，DeepSeek-V4 思考模式执行
- 🐜 **Swarm 编排** — Queen-led 多 Agent 协调
- 🧠 **向量记忆** — 余弦相似度搜索、重要性压缩、文件持久化
- 🔌 **Plugin 系统** — Plugin trait + RegistryHub
- 💻 **Tauri 桌面端** — React/TS 前端 + Rust 后端，系统托盘，单实例锁
- ⌨️ **丰富 CLI** — 10+ 斜杠命令、3 种显示模式（normal/lite/raw）
- 📦 **跨平台发布** — cargo-dist + GitHub Actions（macOS/Linux/Windows）

## 架构

```
                         deepnova-cli (binary)
                              │
         ┌────────────────────┼────────────────────┐
         ▼                    ▼                    ▼
  deepnova-runtime    deepnova-agent       deepnova-provider
  (composition root)  (Runner impl)        (Provider trait)
         │                    │                    │
         └────────────────────┼────────────────────┘
                              │
                       deepnova-core (foundation)
           types / graph / runner / tool / registry
                              │
    ┌─────────┬─────────┬─────┼─────┬─────────┬─────────┬──────────┐
    ▼         ▼         ▼     │     ▼         ▼         ▼          ▼
deepnova-  deepnova-  deepnova-│ deepnova-  deepnova-  deepnova-  deepnova-
config     event      permission│ context    tools      mcp        orch (★)
                                                                    │
                                                   ┌────────────────┤
                                                   ▼                ▼
                                            planner (GOAP)    memory (vector)
                                            swarm (Queen)    plugin system
```

### Workspace Crates（21 crates）

| Crate | 描述 |
|---|---|
| `deepnova-core` | 核心类型系统：Runner、Tool、ExecutionGraph、RegistryHub、Plugin |
| `deepnova-agent` | Agent 循环、工具执行、记忆压缩、规划模式、子 Agent |
| `deepnova-provider` | LLM Provider：OpenAI（SSE + thinking mode）、Anthropic |
| `deepnova-tools` | 13 内置工具 + **Snippet 系统**（read→snippet_id→edit） |
| `deepnova-orch` ★ | 编排层：**GOAP 规划器**、**Swarm 协调器**、**向量记忆** |
| `deepnova-desktop` ★ | **Tauri 2.x 桌面端**（React/TS + Rust 后端） |
| `deepnova-mcp` | MCP 客户端 |
| `deepnova-config` | TOML 分层配置 |
| `deepnova-context` | 工作区索引、项目记忆 |
| `deepnova-permission` | 权限门控（allow/ask/deny） |
| `deepnova-event` | 事件总线 |
| `deepnova-runtime` | 组合根 |
| `deepnova-sandbox` | 进程沙箱（macOS/Linux） |
| `deepnova-checkpoint` | 文件检查点/回滚 |
| `deepnova-store` | JSONL 会话持久化 |
| `deepnova-tui` | ratatui 终端 UI |
| `deepnova-serve` | axum HTTP/SSE 服务器 |
| `deepnova-skills` | Skills 系统（Markdown + YAML） |
| `deepnova-cli` | CLI 二进制 |
| `deepnova-telemetry` | OpenTelemetry |

## 快速开始

### 前置条件

- Rust 1.75+
- DeepSeek API key（设置 `DEEPSEEK_API_KEY` 环境变量）

### 安装

```bash
git clone https://github.com/W117C/DeepNova.git
cd deepnova-rs
cargo build --release
```

### CLI 使用

```bash
# 一次性任务
cargo run -- run "列出 src/ 下所有 Rust 文件"

# 交互式聊天（带斜杠命令）
cargo run -- chat
# > /help  # 查看所有命令
# > /raw   # 切换显示模式

# 高级用法：规划模式
cargo run -- plan "实现用户认证功能" \
  --planner-model deepseek-pro \
  --executor-model deepseek-flash

# HTTP 服务器
cargo run -- serve --port 3000
```

### 桌面端

```bash
cd crates/deepnova-desktop

# 安装前端依赖
cd frontend && npm install && cd ..

# 开发模式
cargo run
```

### 配置

```toml
# deepnova.toml
[[providers]]
name = "deepseek-flash"
kind = "openai"
base_url = "https://api.deepseek.com"
model = "deepseek-v4-flash"
api_key_env = "DEEPSEEK_API_KEY"
thinking_enabled = true    # 启用 DeepSeek 思考模式

[agent]
max_steps = 25
system_prompt = "You are a helpful software engineer."
compaction_threshold_tokens = 32000
```

## DeepSeek-V4 深度集成

| 特性 | 支持 |
|---|---|
| 思考模式（thinking mode） | ✅ `reasoning_effort: "low/medium/high/max"` |
| 思考内容流式 | ✅ `reasoning_content` → `ReasoningDelta` chunk |
| 上下文缓存 | ✅ `cache_hit_tokens` / `cache_miss_tokens` 解析 + 前端显示 |
| 工具调用 + 思考 | ✅ 多轮思考→工具执行循环 |
| `reasoning_content` 回传 | ✅ 工具调用轮次必须回传（否则 400） |
| `extra_body` 扩展 | ✅ `{"thinking": {"type": "enabled"}}` |

## 项目结构

```
.
├── Cargo.toml                    # Workspace 根
├── deepnova.toml                 # 项目配置
├── crates/
│   ├── deepnova-core/            # 核心类型、Runner/Tool/Plugin traits
│   ├── deepnova-agent/           # Agent 循环 + 工具执行
│   ├── deepnova-provider/        # OpenAI/Anthropic Provider
│   ├── deepnova-tools/           # 13 内置工具 + Snippet 系统
│   ├── deepnova-orch/ ★          # GOAP Planner + Swarm + Vector Memory
│   ├── deepnova-desktop/ ★       # Tauri 桌面端
│   ├── deepnova-cli/             # CLI 二进制
│   ├── deepnova-mcp/             # MCP 客户端
│   ├── deepnova-config/          # 配置加载
│   ├── deepnova-context/         # 上下文管理
│   ├── deepnova-permission/      # 权限系统
│   ├── deepnova-event/           # 事件总线
│   ├── deepnova-runtime/         # 组合根
│   ├── deepnova-sandbox/         # 沙箱
│   ├── deepnova-security/        # 安全策略（能力/路径/命令/域名/资源限额）
│   ├── deepnova-checkpoint/      # 检查点
│   ├── deepnova-store/           # 会话持久化
│   ├── deepnova-tui/             # TUI
│   ├── deepnova-serve/           # HTTP 服务器
│   ├── deepnova-skills/          # Skills
│   └── deepnova-telemetry/       # 遥测
├── .github/workflows/            # CI/CD
└── GUIDE.md                      # 用户指南
```

## 开发

```bash
# 测试
cargo test --all --workspace

# 编译检查
cargo check --all-targets --workspace

# Lint
cargo clippy --all-targets --workspace -- -D warnings

# 格式化
cargo fmt --all --check

# 文档
cargo doc --no-deps --workspace --document-private-items
```

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.

## 安全

deepnova 在 agent / coordinator 每一个工具执行点上，通过 [deepnova-security](crates/deepnova-security) 注入可配置的安全策略。策略通过 `deepnova.toml` 或 `~/.deepnova/config.toml` 的 `[security]` 段配置，启动时由 [deepnova-runtime](crates/deepnova-runtime) 组装成 [`SecurityContext`]，并注入每个 `ToolContext` 的 extensions。

### 默认策略（开箱即用）

| 维度 | 默认值 |
|---|---|
| capabilities | 全能力开放 `file_read / file_write / command_execute / network_access / mcp_invoke / memory_read / memory_write` |
| 允许路径 | 仅当前工作区根目录（`cwd`），自动加入 allow-list |
| 命令/域名 | 不限制 |
| 资源限额 | 文件数 500 / 单文件 1 MB / 总读取 50 MB / 执行 120 s / 输出 10 MB / 工具调用 100 次 |

### 配置示例

```toml
# ~/.deepnova/config.toml  或  ./deepnova.toml
[security]
disabled_capabilities = ["command_execute", "network_access"]
allowed_paths  = ["/data/build"]
denied_paths    = ["/data/build/secrets"]
allowed_commands = ["git", "cargo"]
allowed_domains  = ["api.github.com"]

[security.limits]
max_files               = 100
max_file_size           = 1048576      # 1 MB
max_total_read_bytes    = 52428800     # 50 MB
max_execution_time_secs = 60
max_output_bytes        = 10485760     # 10 MB
max_tool_calls          = 50
```

`SecurityContext` 由 [deepnova-runtime](crates/deepnova-runtime) 运行时构建；`disabled_capabilities` 从全能力集合里移除；`allowed_paths` 自动预置工作区根（首条）；`denied_paths` 优先于全部 allow 规则。审计日志通过 `TracingAuditLogger` 输出到 tracing substrate。

[`SecurityContext`]: crates/deepnova-security/src/context.rs
