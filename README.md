<div align="center">

# 🌟 DeepseekNova

### DeepSeek 原生 AI 编程 Agent 框架

**21 个 Rust crate · 47 个 Tauri 命令 · 三端覆盖（CLI / TUI / Desktop）**

Rust 从头构建的 AI Agent 框架，不是套壳—— 是为 DeepSeek 模型量身打造的原生编程助手。

</div>

---

## 🎯 核心特点

### 🧠 深度推理 + 工具调用
- 流式推理输出，支持 Reasoning Effort 四级调节（low / medium / high / max）
- 13+ 内置工具：文件 I/O、glob、grep、shell、web fetch、任务管理、MCP 桥接
- 工具调用全链路流式：start → delta → end → result，前端实时渲染

### ⚡ Prefix-Cache 三层架构
- **会话级缓存** — 跨轮次 prompt prefix 命中，实时统计命中率
- **Token 追踪** — 实时统计输入/输出/推理/缓存 token，精确成本计算
- **预算控制** — 单会话 Token 上限，超额自动停止

### 🔒 安全沙箱 + 权限门控
- **沙箱执行** — macOS Seatbelt / Linux bubblewrap 隔离
- **权限策略** — 12 条规则，每条独立开关：目录沙箱、Plan 模式、Shell 确认、网络访问等
- **安全层** — 路径白名单/黑名单、环境变量隔离、CSP 策略、敏感文件保护

### 🎪 多 Agent 编排
- GOAP 规划器 — 目标导向行动规划
- Swarm 协调 — 多 Agent 集群协作
- Agent Federation — 跨实例联邦调度

### 🧩 MCP 协议原生支持
- stdio + HTTP 双传输
- 自动发现 MCP 服务器工具
- 4 个运行时管理命令（list / add / remove / toggle）

### 📖 项目知识系统
- **Wiki 生成器** — 自动文档生成
- **知识卡片** — 置信度标注的结构化知识
- **记忆蒸馏** — 跨会话记忆持久化（项目 / 用户 / 全局 / 会话四层）
- **文件检查点** — 事务性快照 + 回滚

## 🏗️ 架构

```
┌─────────────────────────────────────────────────────┐
│                   前端层 (Frontend)                    │
│  Desktop (Tauri 2.0 + React 18 + TypeScript + Vite)  │
│  TUI (ratatui)  ·  CLI (clap)                        │
└──────────────────────┬──────────────────────────────┘
                       │ 47 Tauri Commands / IPC
┌──────────────────────┴──────────────────────────────┐
│                  桌面运行时 (Desktop Runtime)          │
│  Tauri 2.0 · 47 Commands · Channel<WireEvent>        │
└──────────────────────┬──────────────────────────────┘
                       │
┌──────────────────────┴──────────────────────────────┐
│                 Agent 运行时 (Runtime)                 │
│  Agent Loop · Coordinator · Plan-Mode Runner         │
│  Event Bus · Permission Gate · Security Context       │
└──────────────────────┬──────────────────────────────┘
                       │
┌──────────────────────┴──────────────────────────────┐
│                  核心层 (Core)                          │
│  Runner Trait · Tool Trait · Registry · RunInput     │
│  WireEvent (text/reasoning/tool/usage/done)          │
└──────────┬───────────────────────┬──────────────────┘
           │                       │
┌──────────┴──────────┐ ┌─────────┴────────────────────┐
│    Provider 层       │ │      工具层 (Tools)           │
│  DeepSeek V4 Pro    │ │  File · Glob · Grep · Shell   │
│  DeepSeek V4 Flash  │ │  WebFetch · Task · MCP Bridge │
│  Streaming + Tools  │ │  13+ Built-in Tools          │
└─────────────────────┘ └──────────────────────────────┘
```

## 📦 21 个 Crate

| Crate | 职责 |
|-------|------|
| `deepseeknova-core` | 核心类型：Runner / Tool trait、Registry、WireEvent |
| `deepseeknova-agent` | Agent 主循环、Coordinator、Plan-Mode Runner |
| `deepseeknova-provider` | DeepSeek / OpenAI 兼容 / Anthropic 流式 Provider |
| `deepseeknova-tools` | 13+ 内置工具实现 |
| `deepseeknova-mcp` | MCP 协议客户端（stdio / HTTP） |
| `deepseeknova-sandbox` | 沙箱 trait + macOS Seatbelt / Linux bubblewrap |
| `deepseeknova-permission` | Allow / Ask / Deny 权限门控 |
| `deepseeknova-security` | 路径限制、资源限额、审计日志 |
| `deepseeknova-checkpoint` | 文件系统快照 + 事务性回滚 |
| `deepseeknova-context` | 工作区索引、项目记忆、会话状态 |
| `deepseeknova-skills` | Markdown 技能系统，兼容 .claude/skills 格式 |
| `deepseeknova-store` | JSONL 会话持久化 + 轮转 + 压缩 |
| `deepseeknova-orch` | GOAP 规划、Swarm 协调、Agent 联邦 |
| `deepseeknova-telemetry` | OpenTelemetry 分布式追踪 (OTLP) |
| `deepseeknova-event` | Agent 生命周期事件总线 |
| `deepseeknova-runtime` | 组合根：注册表 + 上下文 + 事件 + 权限 + 安全 |
| `deepseeknova-config` | 分层 TOML 配置（默认 → 用户 → 项目 → 环境变量 → CLI） |
| `deepseeknova-cli` | CLI 前端：chat / plan / serve / setup |
| `deepseeknova-tui` | ratatui 终端 UI |
| `deepseeknova-serve` | axum HTTP 服务器 + SSE 流式 |
| `deepseeknova-desktop` | Tauri 2.0 桌面应用 + React 前端 |

## 🖥️ 三端覆盖

| 端 | 技术 | 特点 |
|----|------|------|
| **CLI** | clap | 轻量，单二进制，chat / plan / serve / setup |
| **TUI** | ratatui | 全屏终端 UI，快捷键驱动 |
| **Desktop** | Tauri 2.0 + React 18 | 原生桌面体验，47 个 IPC 命令 |

### 桌面前端亮点

- **三栏布局** — 会话列表 / 消息流 / 右侧面板
- **设置面板** — 14 大分区：通用 / 外观 / 执行 / 快捷键 / 沙箱 / 网络 / 权限 / 钩子 / MCP / 子智能体 / 诊断 / 账单 / 技能 / 更新
- **右侧面板** — 5 标签页：文件（修改/创建/读取三区） / 知识库（Wiki + 卡片 + 记忆） / 工具（MCP + 技能） / 记忆 CRUD / 权限规则
- **流式渲染** — 文本 / 推理 / 工具调用 / 审批 全链路流式
- **三色进度条** — 缓存命中（绿） / 未缓存（黄） / 剩余（灰）

## 🚀 快速开始

### 安装

```bash
# 从源码构建 CLI
cargo build --release -p deepseeknova-cli

# 桌面端
cd crates/deepseeknova-desktop/frontend
npm ci && npm run build
cargo build -p deepseeknova-desktop
```

### 配置

```toml
# ~/.deepseeknova/config.toml
default_model = "deepseek-chat"

[[providers]]
name = "deepseek"
kind = "openai-compatible"
base_url = "https://api.deepseek.com/v1"
api_key = "your-api-key"
model = "deepseek-chat"
```

### 使用

```bash
# CLI
deepseeknova chat
deepseeknova plan --prompt "重构这个模块"
deepseeknova serve --port 8080

# TUI
deepseeknova chat --tui

# Desktop
deepseeknova desktop
```

## 📊 CI 状态

| 检查项 | 状态 |
|--------|------|
| cargo check (全 workspace) | ✅ |
| cargo check (desktop + Tauri) | ✅ |
| cargo clippy (-D warnings) | ✅ |
| cargo fmt | ✅ |
| cargo test (Ubuntu / macOS / Windows) | ✅ |
| cargo doc | ✅ |
| cargo llvm-cov (覆盖率) | ✅ |
| cargo bench (基准测试) | ✅ |
| cargo audit + cargo deny (安全审计) | ✅ |
| frontend build (TypeScript + Vite) | ✅ |
| release build (Linux / macOS / Windows) | ✅ |

## 🛠️ 技术栈

| 层 | 技术 |
|----|------|
| 语言 | Rust (stable 1.97) + TypeScript |
| 后端 | Rust + SQLite FTS5 + tokio + axum |
| 前端 | React 18 + Vite 5 + Zustand |
| 桌面 | Tauri 2.0 |
| 追踪 | OpenTelemetry (OTLP) |
| 测试 | 382+ tests · cargo-llvm-cov · CI 三平台 |

## 📄 License

MIT OR Apache-2.0
