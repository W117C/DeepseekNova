<div align="center">

# 🌟 DeepseekNova

### DeepSeek 原生 AI 编程 Agent 框架

**22 个 Rust crate · 三端覆盖（CLI / TUI / HTTP API）**

Rust 从头构建的 AI Agent 框架，不是套壳—— 是为 DeepSeek 模型量身打造的原生编程助手。

[English](README_EN.md) | [中文](README.md)

</div>

---

<!-- CI Badges (auto-updated from GitHub Actions) -->

<div align="center">

[![CI](https://github.com/W117C/DeepseekNova/actions/workflows/ci.yml/badge.svg)](https://github.com/W117C/DeepseekNova/actions/workflows/ci.yml)
[![Security](https://github.com/W117C/DeepseekNova/actions/workflows/security.yml/badge.svg)](https://github.com/W117C/DeepseekNova/actions/workflows/security.yml)
[![Release](https://github.com/W117C/DeepseekNova/actions/workflows/release.yml/badge.svg)](https://github.com/W117C/DeepseekNova/actions/workflows/release.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![Rust](https://img.shields.io/badge/rust-stable%201.97-orange.svg)](https://www.rust-lang.org)
[![Crates](https://img.shields.io/badge/crates-22-green.svg)](#-22-个-crate)
[![Tests](https://img.shields.io/badge/tests-1571-brightgreen.svg)](#-技术栈)

</div>

---

## 📸 截图

> 截图待补 —— 可运行 `deepseeknova-cli chat --tui` 体验终端 UI（需先配置 `DEEPSEEK_API_KEY`）。

---

## 🎯 核心特点

### 🧠 深度推理 + 工具调用
- 流式推理输出，支持 Reasoning Effort 三级调节（disabled / high / max；配置串 `low`/`medium` 折叠为 high）
- 17 个内置工具 + web 搜索 + LSP 编辑后诊断 + Context7 文档检索：文件 I/O、
  glob、grep、shell、web fetch、任务管理、MCP 桥接、代码图等
- 工具调用全链路流式：start → delta → end → result，前端实时渲染
- **编辑后诊断** — write/edit/move 成功后自动调用语言服务器
  （rust-analyzer / pyright / gopls / typescript-language-server / clangd）
  并把诊断注入上下文，模型改完立刻看到编译/类型错误

### 🧭 Auto 模型 + 思考路由
- `[agent] auto_route = true` 时，每轮新用户消息先用廉价模型决定
  flash/pro 与 thinking off/high/max，再执行真实调用；工具续步复用决策，
  路由失败自动回退启发式/默认模型，不影响主流程

### ⚡ Prefix-Cache（Provider 级）
- **API 级前缀缓存** — DeepSeek V4 磁盘级自动前缀缓存（字节级前缀命中
  即复用）；`cache_hit_tokens` / `cache_miss_tokens` 来自 provider 返回的
  usage，逐请求透传
- **会话级命中率统计** — [规划中]：跨轮次 prompt prefix 命中率的实时统计
  尚未落地（Usage 相关字段当前恒为 0）
- **Token 追踪** — 单请求级输入/输出/推理/缓存 token 实时统计，精确成本计算
- **预算控制** — 单会话 Token 上限，超额自动停止

### 🧭 决策引擎式系统提示词
- 内置英文默认系统提示词：把 DeepSeek-V4-Flash 当作低成本高频决策引擎，按
  Observe → Plan → Tool → Verify → Reflect → Next Action 循环工作
- 未配置 `system_prompt` 时自动启用，配置后完全覆盖；规划器/子代理/审查/压缩/
  安全调查等全链路提示词使用同一套循环术语，机器契约不变

### 🔒 安全沙箱 + 权限门控
- **沙箱执行** — macOS Seatbelt / Linux bubblewrap 隔离
- **权限策略** — allow/ask/deny 规则门控（deny > ask > allow > 默认模式）+
  会话缓存 + 可选速率限制；shell 只读命令四层分类器（任意参数安全 / 零参 /
  精确形式 / 子命令 flag 白名单）免询问放行；普通链式/重定向/命令替换按非只读
  走权限审批/规则，工具级注入面（`git -c`/`--config-env`、格式串注入、
  UNC/URL/SMB 路径形态等）硬拒且不可被规则覆盖
- **安全层** — 路径/命令/域名策略、资源限额、审计日志、敏感文件质量规则
  （no-commit-secret / no-forbidden-path）

> ⚠️ **Windows 安全边界**：当前沙箱隔离仅支持 macOS (Seatbelt) 和 Linux (bubblewrap)。Windows 平台执行 Shell 工具时使用 `NoOpSandbox`（无隔离），后续计划通过 Job Object / AppContainer 补齐。在 Windows 平台上，请谨慎配置 `allowed_commands` 和路径策略。

### 🎪 多 Agent 委派
- **delegate 子代理** — explorer / coder / tester / reviewer 四类预设，受限工具集 + 信号量并发控制 + 结果封顶回传
- 独立上下文隔离，禁递归（子代理不能再委派）

> 🔬 历史 GOAP / Swarm / Federation 实验已于 B0 裁撤（见 DESIGN.md）；多智能体能力现由 delegate 路径提供。

### 🧩 MCP 协议原生支持
- stdio + HTTP 双传输
- 自动发现 MCP 服务器工具
- `/mcp` 运行时管理命令（列表 + 连接状态探测；add/remove 等管理见
  [GUIDE.md](GUIDE.md)）

### 📖 项目知识系统
- **Wiki 生成器** — 自动文档生成
- **知识卡片** — 置信度标注的结构化知识
- **记忆蒸馏** — 跨会话记忆持久化（短期 / 任务 / 技能 / 用户画像四类：
  ShortTerm · Task · Skill · UserProfile）
- **文件检查点** — 事务性快照 + 回滚

### 🧬 协议执行引擎与自进化（`[protocol]`）
- **DNA 五阶段门控** — Understand→Plan→Execute→Verify→Distill 运行时门控，内置
  plan-before-execute / verify-evidence / distill-on-complex / drift-detection
  四门，`hard|soft|off` 三力度可配，默认关闭零开销
- **验证证据锚定** — verify 配置且零 passed → Blocking 拒绝；无证据 Complete →
  诊断报告标注 `unverified`；对抗审查子代理按条件自动委派
- **技能自进化** — 技能使用/成功率持久化（fitness），自动给出
  淘汰/合并/置顶建议，deprecated 标记过滤
- **失败模式回灌** — 历史失败聚类入库（脱敏 + 0600），每次会话 top-3 自动注入
  首轮 system prompt，同类失败不再重犯

## 🏗️ 架构

```
┌─────────────────────────────────────────────────────┐
│                   前端层 (Frontend)                    │
│  TUI (ratatui)  ·  CLI (clap)  ·  HTTP API (serve)  │
└──────────────────────┬──────────────────────────────┘
                       │ WireEvent / SSE / CLI 输出
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
│  Streaming + Tools  │ │  17 Built-in Tools           │
└─────────────────────┘ └──────────────────────────────┘
```

## 📦 22 个 Crate

| Crate | 职责 |
|-------|------|
| `deepseeknova-core` | 核心类型：Runner / Tool trait、Registry、WireEvent |
| `deepseeknova-agent` | Agent 主循环、Coordinator、Plan-Mode Runner |
| `deepseeknova-provider` | DeepSeek / OpenAI 兼容 / Anthropic 流式 Provider |
| `deepseeknova-tools` | 17 个内置工具 + web 搜索 + LSP 诊断 + Context7 文档检索 |
| `deepseeknova-mcp` | MCP 协议客户端（stdio / HTTP） |
| `deepseeknova-metrics` | 会话级效能度量 + 评分卡（四维 + protocol/composite）落盘 |
| `deepseeknova-graph` | 代码图检索引擎（tree-sitter + SQLite FTS5 + PageRank + repo map） |
| `deepseeknova-sandbox` | 沙箱 trait + macOS Seatbelt / Linux bubblewrap |
| `deepseeknova-permission` | Allow / Ask / Deny 权限门控 |
| `deepseeknova-security` | 路径限制、资源限额、审计日志、质量规则、失败模式库 |
| `deepseeknova-scanner` | deepsec 式安全扫描：规则匹配 + 可选 AI 调查（`scan` 子命令） |
| `deepseeknova-checkpoint` | 文件系统快照 + 事务性回滚 |
| `deepseeknova-context` | 工作区索引、项目记忆、会话状态 |
| `deepseeknova-skills` | Markdown 技能系统，兼容 .claude/skills 格式 |
| `deepseeknova-store` | JSONL 会话持久化 + 轮转 + 压缩 |
| `deepseeknova-telemetry` | OpenTelemetry 分布式追踪 (OTLP) |
| `deepseeknova-event` | Agent 生命周期事件总线 |
| `deepseeknova-runtime` | 组合根：注册表 + 上下文 + 事件 + 权限 + 安全 |
| `deepseeknova-config` | 分层 TOML 配置（默认 → 用户 → 项目 → 环境变量 → CLI） |
| `deepseeknova-cli` | CLI 前端：chat / plan / serve / setup |
| `deepseeknova-tui` | ratatui 终端 UI |
| `deepseeknova-serve` | axum HTTP 服务器 + SSE 流式 |

## 🖥️ 三端覆盖

| 端 | 技术 | 特点 |
|----|------|------|
| **CLI** | clap | 轻量，单二进制，chat / plan / scan / serve / setup |
| **TUI** | ratatui | 全屏终端 UI，快捷键驱动 |
| **HTTP API** | axum + SSE | 无头服务，WireEvent 流式输出，可对接任意前端 |

## 🚀 快速开始

### 安装

```bash
# 从源码构建 CLI
cargo build --release -p deepseeknova-cli
```

### 配置

推荐使用环境变量注入密钥，避免将 API key 写入配置文件：

```bash
# 推荐方式：通过环境变量
export DEEPSEEK_API_KEY="your-api-key"
```

```toml
# ~/.deepseeknova/config.toml
default_model = "deepseek-chat"

[[providers]]
name = "deepseek"
kind = "openai-compatible"
base_url = "https://api.deepseek.com/v1"
# 从环境变量读取，不硬编码到配置文件
api_key_env = "DEEPSEEK_API_KEY"
model = "deepseek-chat"
```

> 💡 `DEEPSEEK_API_KEY` 是代码默认读取的环境变量名（`api_key_env` 未配置时），
> 也支持 `api_key` 字段直接写入，但 **不推荐**——容易误提交到版本控制。
> 其他 provider（如 Anthropic）默认读 `ANTHROPIC_API_KEY`。

### 使用

```bash
# CLI
deepseeknova-cli chat
deepseeknova-cli plan "重构这个模块"
deepseeknova-cli serve --addr 127.0.0.1:8080
deepseeknova-cli serve --acp        # Agent Client Protocol stdio 模式
deepseeknova-cli eval evals.jsonl   # 跑最小 eval 集（JSONL + must_contain）

# TUI
deepseeknova-cli chat --tui
```

`deepseeknova-cli serve` 会把每次 run 持久化到工作区 `.deepseeknova/runs/`：
`GET /v1/runs` 列出任务，`POST /v1/runs/{id}/resume` 恢复（服务重启后
running 任务自动标记 interrupted，可重新拉起）。

## 📊 CI 检查项

以下检查在每次 push / PR 时自动运行，点击徽章查看详情：

| 检查项 | 工作流 |
|--------|--------|
| cargo check (全 workspace) | CI |
| cargo clippy (-D warnings) | CI |
| cargo fmt | CI |
| cargo test (Ubuntu / macOS / Windows) | CI |
| cargo doc | CI |
| cargo llvm-cov (覆盖率) | CI |
| cargo bench (基准测试) | CI |
| release build (Linux / macOS / Windows) | Release |
| cargo audit + cargo deny (安全审计) | Security |

## 🛠️ 技术栈

| 层 | 技术 |
|----|------|
| 语言 | Rust (stable 1.97) |
| 后端 | Rust + SQLite FTS5 + tokio + axum |
| 前端 | TUI (ratatui) · CLI (clap) · HTTP API (axum + SSE) |
| 追踪 | OpenTelemetry (OTLP) |
| 测试 | 1571 tests · cargo-llvm-cov · CI 三平台 |

## 📄 License

MIT OR Apache-2.0 — 见 [LICENSE-MIT](LICENSE-MIT) 和 [LICENSE-APACHE](LICENSE-APACHE)。
