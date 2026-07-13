# DPronix — DeepSeek-V4 原生 AI 编码代理框架

[![CI](https://github.com/W117C/DPronix/actions/workflows/ci.yml/badge.svg)](https://github.com/W117C/DPronix/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE)

**DPronix**（原名 dpronix-rs）是一个深度适配 **DeepSeek-V4** 的模块化 AI 编码代理框架，支持思考模式、上下文缓存、工具调用和多 Agent 编排。提供 CLI、TUI、HTTP API 和 **Tauri 桌面端**四种交互方式。

> 从 esengine/DeepSeek-DPronix、deepcode-cli、Ruflo、ECC 等顶级项目吸取设计，围绕 DeepSeek 的 prefix cache 和 thinking mode 深度优化。

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
                         dpronix-cli (binary)
                              │
         ┌────────────────────┼────────────────────┐
         ▼                    ▼                    ▼
  dpronix-runtime    dpronix-agent       dpronix-provider
  (composition root)  (Runner impl)        (Provider trait)
         │                    │                    │
         └────────────────────┼────────────────────┘
                              │
                       dpronix-core (foundation)
           types / graph / runner / tool / registry
                              │
    ┌─────────┬─────────┬─────┼─────┬─────────┬─────────┬──────────┐
    ▼         ▼         ▼     │     ▼         ▼         ▼          ▼
dpronix-  dpronix-  dpronix-│ dpronix-  dpronix-  dpronix-  dpronix-
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
| `dpronix-core` | 核心类型系统：Runner、Tool、ExecutionGraph、RegistryHub、Plugin |
| `dpronix-agent` | Agent 循环、工具执行、记忆压缩、规划模式、子 Agent |
| `dpronix-provider` | LLM Provider：OpenAI（SSE + thinking mode）、Anthropic |
| `dpronix-tools` | 13 内置工具 + **Snippet 系统**（read→snippet_id→edit） |
| `dpronix-orch` ★ | 编排层：**GOAP 规划器**、**Swarm 协调器**、**向量记忆** |
| `dpronix-desktop` ★ | **Tauri 2.x 桌面端**（React/TS + Rust 后端） |
| `dpronix-mcp` | MCP 客户端 |
| `dpronix-config` | TOML 分层配置 |
| `dpronix-context` | 工作区索引、项目记忆 |
| `dpronix-permission` | 权限门控（allow/ask/deny） |
| `dpronix-event` | 事件总线 |
| `dpronix-runtime` | 组合根 |
| `dpronix-sandbox` | 进程沙箱（macOS/Linux） |
| `dpronix-checkpoint` | 文件检查点/回滚 |
| `dpronix-store` | JSONL 会话持久化 |
| `dpronix-tui` | ratatui 终端 UI |
| `dpronix-serve` | axum HTTP/SSE 服务器 |
| `dpronix-skills` | Skills 系统（Markdown + YAML） |
| `dpronix-cli` | CLI 二进制 |
| `dpronix-telemetry` | OpenTelemetry |

## 快速开始

### 前置条件

- Rust 1.75+
- DeepSeek API key（设置 `DEEPSEEK_API_KEY` 环境变量）

### 安装

```bash
git clone https://github.com/W117C/DPronix.git
cd dpronix-rs
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
cd crates/dpronix-desktop

# 安装前端依赖
cd frontend && npm install && cd ..

# 开发模式
cargo run
```

### 配置

```toml
# dpronix.toml
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
├── dpronix.toml                 # 项目配置
├── crates/
│   ├── dpronix-core/            # 核心类型、Runner/Tool/Plugin traits
│   ├── dpronix-agent/           # Agent 循环 + 工具执行
│   ├── dpronix-provider/        # OpenAI/Anthropic Provider
│   ├── dpronix-tools/           # 13 内置工具 + Snippet 系统
│   ├── dpronix-orch/ ★          # GOAP Planner + Swarm + Vector Memory
│   ├── dpronix-desktop/ ★       # Tauri 桌面端
│   ├── dpronix-cli/             # CLI 二进制
│   ├── dpronix-mcp/             # MCP 客户端
│   ├── dpronix-config/          # 配置加载
│   ├── dpronix-context/         # 上下文管理
│   ├── dpronix-permission/      # 权限系统
│   ├── dpronix-event/           # 事件总线
│   ├── dpronix-runtime/         # 组合根
│   ├── dpronix-sandbox/         # 沙箱
│   ├── dpronix-checkpoint/      # 检查点
│   ├── dpronix-store/           # 会话持久化
│   ├── dpronix-tui/             # TUI
│   ├── dpronix-serve/           # HTTP 服务器
│   ├── dpronix-skills/          # Skills
│   └── dpronix-telemetry/       # 遥测
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
