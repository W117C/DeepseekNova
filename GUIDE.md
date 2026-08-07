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
cd deepseeknova
cargo build --release
```

The binary is at `target/release/deepseeknova-cli`.

### Initialize a Project

```bash
deepseeknova-cli init
```

`deepseeknova-cli init` 会在当前目录创建：
```
├── DEEPSEEKNOVA.md      # 项目上下文模板
├── deepseeknova.toml    # 项目配置（Config::load 项目层）
└── .deepseeknova/
    ├── commands/        # 自定义斜杠命令（含 build.md 示例）
    └── memory/          # 记忆库目录
```

### Setup Wizard

```bash
deepseeknova-cli setup
```

Walks through provider selection, API key configuration, and tool preferences.

## Configuration

Configuration is merged from multiple sources (last wins):

1. **Built-in defaults**
2. **User config**: `~/.deepseeknova/config.toml`
3. **Project config**: `./deepseeknova.toml`（项目根，覆盖用户层非默认字段）
4. **Environment variables**: `DEEPSEEKNOVA_MODEL`, `DEEPSEEKNOVA_MAX_STEPS`, etc.

### Full Configuration Reference

```toml
# deepseeknova.toml（项目根）或 ~/.deepseeknova/config.toml（用户层）

[[providers]]                     # 注意：是 providers 列表，不是 [default_provider]
name = "openai"                   # 唯一名，被 [[models]] 的 provider 字段引用
kind = "openai"                   # openai | anthropic | ollama | deepseek-anthropic
base_url = "https://api.openai.com/v1"
model = "gpt-4o"
api_key_env = "OPENAI_API_KEY"
timeout_secs = 120
max_retries = 3
reasoning_effort = "high"         # disabled | high | max（low/medium 配置串折叠为 high，DeepSeek 系）
context_window = 128000

# 命名模型与成本分账（可选）：被 model_pointers 与 /model 引用
[[models]]
name = "gpt-4o"
provider = "openai"
context_window = 128000
input_price_per_mtok = 2.5
output_price_per_mtok = 10.0

[agent]
max_steps = 25                     # Max tool-calling iterations per turn
system_prompt = "You are a helpful software engineer."
compaction_threshold = 32000       # Tokens before memory compaction
concurrent_tools = true            # 同批读类工具并发、写类保序串行（P1）
# step_effort_routing = true       # 每步在 quick（thinking off）/ high 间切换（P2）
# auto_route = true                # Auto 模型+思考路由：每轮先由廉价模型决定
#                                   # flash/pro 与 thinking off/high/max（默认关）
# auto_router_model = "deepseek-v4-flash"   # 路由决策用模型（默认 quick 指针）
# auto_router_max_chars = 6000     # 路由调用送入的最新用户消息上限字符数
# observe_compress = true          # 超阈值工具输出由廉价模型摘要后入历史（P2）
# observe_compress_threshold_chars = 12000
# observe_compress_max_chars = 4000
# tool_cache = true                # 会话内只读工具结果缓存，写后失效（P2）

[tools]                            # 按工具名覆盖（name/disabled/timeout_secs/max_file_size）
[[tools.overrides]]
name = "bash"
timeout_secs = 120
# [[tools.overrides]]
# name = "read_file"
# max_file_size = 1048576

# [tools.web_search]               # web 搜索工具（web_search）
# provider = "ddg"                 # ddg（默认，免 key）| tavily | bing | searxng
# base_url = "http://localhost:8888"   # searxng 必填；其它 provider 留空用官方端点
# api_key_env = "TAVILY_API_KEY"   # tavily/bing 需要
# max_results = 5
# timeout_secs = 30

# [tools.lsp]                      # LSP 编辑后诊断（lsp_diagnostics）
# enabled = true                   # 写文件后自动诊断（默认开）
# timeout_secs = 8                 # 等待诊断的超时
# max_file_bytes = 1048576
# [tools.lsp.servers]              # 语言 → 服务器覆盖
# rust = "rust-analyzer"

[sandbox]                          # shell 命令 OS 级沙箱（档位在运行时按平台选择）
enabled = false                    # 默认关闭；开启后后端缺失（macOS sandbox-exec /
                                   # Linux bwrap）时 shell 执行 fail-closed 拒绝
writable_paths = []                # 额外可写根（工作区默认可写）
allow_network = false              # 默认禁网（ReadOnly/WorkspaceWrite 档强制）

[permissions]
enabled = true                     # false（默认）时工具不经过 allow/ask/deny 门控
default_mode = "ask"               # 无规则命中时写工具的默认行为：ask | allow | deny
# rate_limit_per_minute = 30       # 可选：滚动 60s 窗口内的门控调用上限

# 规则按顺序匹配，deny > ask > allow > default_mode（subject 可省略，见下文
# "Permission Policies" 节完整示例）
[[permissions.rules]]
tool = "bash"
mode = "ask"

mcp_servers = [
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
llm = false                           # 默认 false：命令通过后再用 LLM 判定产出是否满足任务
llm_model = "deepseek-v4-flash"       # 可选；未配置回落 main provider
llm_max_chars = 4000                  # 送入验证的完成文本上限（字符）
```

验证通过后继续原有流程（B3 自审 / Done）。命令需同时满足 `[security]` 的
`allowed_commands`（启用时），未命中白名单会作为验证失败处理。

`llm = true` 时，确定性命令全部通过后（或未配置命令时）会用 `llm_model` 再判定一次
完成文本是否真正满足任务：模型明确判定失败才回炉，调用/解析失败优雅跳过（不阻断
Done）。成本敏感场景保持默认关闭，用确定性命令即可。

验证命令的逐条结果通过 `verification` 事件推送给前端（TUI 显示为 `✓ / ✗` 系统行，
HTTP API 为 `event: verification` 的 SSE）。

### 失败回炉反思（`[agent]`）

P1 验证或 B3 审查失败需要回炉修复时，Agent 会先用 LLM 做一次显式反思——分析根因与
修复计划——再把反思前置到回炉消息里让模型带着计划去修；反思提炼的教训（lesson）会
沉淀进记忆库（Skill 类目，去重 + 脱敏），下次任务可被召回复用。反思只发生在失败回炉
路径，调用失败或响应不可解析时静默回落原文案，不阻断循环：

```toml
[agent]
reflect_on_failure = true            # 默认 true（失败路径本就昂贵；可关）
reflect_model = "deepseek-v4-flash"  # 可选；未配置回落 main provider
reflect_max_chars = 4000             # 反思输入的最后完成文本上限（字符）
```

反思契约：模型返回 `{"root_cause":"...","fix_plan":"...","lesson":"..."}`。

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
（search_code / traverse_graph / retrieve_entity / trace_code / impact_code /
explore_code）对执行器可用，只读工具对规划器开放；`[graph] enabled = false` 时自动排除。

### 委派子代理（`[delegate]`）

内置 explorer / coder / tester / reviewer 四类预设可经配置覆盖或新增；`inputs`
为参数化任务书默认值（`${{ inputs.<name> }}` 占位符），调用方传值优先：

```toml
[delegate]
enabled = true                     # false = 不注册 delegate 工具
max_concurrent = 2                 # 并发子代理上限，满员排队
output_cap_tokens = 2000           # 回传摘要 token 上限

[[delegate.agents]]
name = "coder"
max_steps = 25
tools = ["read_file", "write_file", "edit_file", "bash"]

[[delegate.agents.inputs]]
name = "path"
value = "src/lib.rs"
```

### 失败归因重试（`[attribution]`）

子代理失败 / verify / review 达上限 Paused 前，可选 LLM 归因（Retry / Degrade /
Abort）后再决定重试；默认关闭，开启后受硬预算约束防烧 token：

```toml
[attribution]
enabled = false                    # 默认 false：关闭时失败直接上抛
max_retries = 1                    # Retry/Degrade 重试次数上限（共 2 次尝试）
max_attributions = 3               # 单次 run 内归因调用次数上限

[attribution.degrade_map]
researcher = "explorer"            # Degrade 时换用目标预设；未映射按 Retry 处理
```

### 任务质量闭环（`[quality]`）

工具调用生命周期治理与写后策略评估（默认开启）：每次工具调用前后经 ToolHook 链
（core 定义、可注册多个）观察/建议/拦截，与 permission gate 合并裁决——任一
Deny 拒绝执行；无 Deny 且任一 Ask 走人工审批（`/v1/approval`）；全 Allow 放行。
写类工具执行后先跑 0 token 确定性规则（内置 no-commit-secret /
no-forbidden-path / oversized-write，路径规则匹配时大小写归一），命中 Blocking 级
finding 才升级 LLM 自审，否则跳过以省 token：

```toml
[quality]
enabled = true    # 默认 true；false 时质量钩子关闭（写后策略评估与 Blocking 短路自审不生效），metrics/评分卡仍由 [metrics] 开关控制
```

`before`/`interested` panic 按 fail-closed 处理（拒绝执行，warn 注明 panic 来源）；
`after` panic 按空 findings 处理（不阻断执行）。bash 写路径启发式：重定向写敏感
路径（如 `.env`）在 before 拒绝、after 记为 Warning。诊断报告与评分卡落盘见
「会话效能度量（SessionMetrics）」节，HTTP 查询见 HTTP API 节。

### 协议增强能力包（`[protocol]`）

DNA 五阶段（Understand→Plan→Execute→Verify→Distill）运行时门控与配套能力包，
默认关闭（`enabled=false` 时行为与现状完全一致，零开销路径）：

```toml
[protocol]
enabled = true                       # 默认 false：总开关，门控/回灌/失败聚类/fitness 均挂此键
adversarial_review = true            # 默认 false：会话结束委派对抗审查子代理

[protocol.gates]
plan-before-execute = "soft"         # 进 Execute 前无任何计划性文本 → Warning
verify-evidence = "hard"             # 已配置 verify 且零 passed → Blocking（默认 hard）
distill-on-complex = "soft"          # 复杂会话（工具调用 >20）无反思记录 → Warning
drift-detection = "soft"             # 工具族连续失败 ≥3 → DriftFinding；同会话第二次 → Warning 违规
```

- 力度语义：`hard` 把 Warning 级违规提升为 Blocking（走工具层 `gate_block` 拒绝
  路径，工具结果回填 `blocked by protocol gate`，保住 replay 不变量）；`soft`
  按门语义 severity 进事件流并注入下轮 prompt；`off` 完全关闭该门（drift 的
  计数/事件/二次违规一并关闭）。缺省条目用内置默认表（前三 soft、
  verify-evidence hard），未知门名 warn 忽略。
- verify-evidence 判定复用 Verification 事件：未配置 verify → 通过；已配置且
  零 Verification 事件（bash 缺失/取消）→ Info 降级；已配置、有失败且无后续
  passed → Blocking。会话 Complete 且 verify-evidence 未通过时，诊断报告
  `outcome` 标注 `unverified`。
- 对抗审查子代理（任一条件触发，独立开关）：① 会话内存在 Blocking 级 finding；
  ② 敏感工具调用叠加 marker（bash/shell 需叠加 sudo/chmod/chown 等敏感词，
  write/edit 需叠加敏感路径，delete/move 无条件）。子代理无 Skill 可用时优雅跳过。
- 度量：Scorecard 新增 `protocol` 维（1 − 门违规数/阶段迁移数，无数据按 1.0）
  与 `composite` 维（五维加权均值：governance 0.30 / verification 0.25 /
  protocol 0.20 / reflection 0.15 / review 0.10）；旧评分卡文件缺字段时反序列化
  默认 1.0（不重算 composite）。
- task_rate 指标：评分卡扩展字段 `first_pass: bool` / `retry_rounds: u32`
  （serde default，旧文件兼容）——成功会话按 `first_pass=true` 填写；失败/
  Paused 会话由诊断回调按 `DiagnoseReport.failures` 推导覆写（无失败=首过；
  有失败=重试轮次=failures 条数）。
- 新落盘路径：技能使用记录 `.deepseeknova/skills/fitness.json`（容量 500 LRU，
  deprecated 标记的技能加载时过滤；会话注入的技能记 `use`+`result`——激活
  计数与会话成败，注入技能名由 recall 注入侧收集回填，无注入时优雅跳过）；
  失败模式库
  `.deepseeknova/security/failure-patterns.json`（容量 200 LRU、脱敏 + 0600，
  每次会话 `suggest(3)` 取 top-3 注入下会话首轮 system prompt，无模式时零注入）。

### Environment Variables

| Variable | Description |
|---|---|
| `OPENAI_API_KEY` | OpenAI API key |
| `ANTHROPIC_API_KEY` | Anthropic API key |
| `DEEPSEEKNOVA_MODEL` | Override model name |
| `DEEPSEEKNOVA_MAX_STEPS` | Override max steps |
| `DEEPSEEKNOVA_EMBED_API_KEY` | Embedder API key（记忆语义检索） |
| `DEEPSEEKNOVA_THEME` | TUI 主题 |

> 注：CLI 日志固定 INFO 级（`crates/deepseeknova-cli/src/main.rs`），**不读取**
> `RUST_LOG` 或任何日志环境变量；开启 `[telemetry] enabled=true` 时日志改走
> OTLP 导出、终端不再打印 INFO 文本（刻意权衡，见 AGENTS.md §3.1）。

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
| `bash` | Execute a shell command | No |

### Web

| Tool | Description | Read-only |
|---|---|---|
| `web_fetch` | Fetch and parse a URL | Yes |
| `web_search` | Search the web (DuckDuckGo / Tavily / Bing / SearXNG) | Yes |

### Diagnostics

| Tool | Description | Read-only |
|---|---|---|
| `lsp_diagnostics` | Run the language server on a file and return diagnostics | Yes |

`lsp_diagnostics` 支持 `rust-analyzer` / `pyright-langserver` / `gopls` /
`typescript-language-server` / `clangd`，语言按扩展名自动识别（可用
`language` 参数覆盖）。write/edit/move 执行成功后 Agent 会自动调用并把结果
注入工具输出；服务器未安装时静默跳过（不会报错打断）。

### Library Docs

| Tool | Description | Read-only |
|---|---|---|
| `context7_docs` | Fetch latest third-party library docs snippets from Context7 | Yes |

参数：`library`（库名，如 `serde`）、`query`（主题，如 `derive Serialize`）、
`library_id`（可选，Context7 库 id，形如 `/serde-rs/serde`，提供后跳过搜索）、
`max_chars`（输出上限，默认 6000）。文档来自 context7.com 公开索引的官方仓库与
网站；无需 API key。端点固定 `context7.com`，执行需网络能力；可用
`[tools] overrides` 把 `context7_docs` 标记为 disabled 来禁用（与其它内置工具同机制）。

### Memory

| Tool | Description | Read-only |
|---|---|---|
| `remember` | Store a fact in persistent memory | No |
| `forget` | Remove a fact from memory | No |
| `recall` | Search persistent memory | Yes |

### 长期记忆与知识蒸馏（`[memory]`）

记忆库持久化在工作区 `.deepseeknova/memory.db`（SQLite + FTS5），跨任务、跨会话
复用：回合开始时按当前任务召回相关记忆注入上下文，运行中（压缩后/新一轮开头）
可再次注入；回合结束自动沉淀任务摘要、失败教训与文件关联（启发式，脱敏 + 内容
去重）。

```toml
[memory]
auto_learn = true                  # 回合结束自动沉淀（默认 true）
embedder = "none"                  # 嵌入后端：none | remote（默认 none）
embed_model = ""                   # embedder=remote 时必填（如 text-embedding-3-small）
embed_base_url = "https://api.openai.com/v1"  # 远程嵌入服务基础 URL
embed_timeout_secs = 30            # 嵌入请求超时（秒）
llm_distill = false                # 默认 false：启用后用 LLM 把任务观察蒸馏成
                                   # 可复用 skill/教训（Skill 类目，去重 + 脱敏）
llm_distill_model = "deepseek-v4-flash"  # 可选；未配置回落 main provider
llm_distill_max_chars = 3000       # 蒸馏输入的任务描述上限（字符）
decay_rate = 0.1                   # memory cleanup 时非 permanent 记忆的衰减率
archive_ttl_days = 30              # archived 记忆距最后召回超过该天数即被清理删除
rank_lifecycle_weight = 0.3        # 检索排序生命周期融合权重；0 = 纯 bm25
```

蒸馏契约：模型返回 `{"kind":"skill"|"lesson","title":"...","body":"...","tags":[...]}`；
调用失败或响应不可解析时静默跳过（启发式沉淀仍照常兜底），不阻断运行。

**记忆生命周期闭环**：每条记忆经历 candidate → verified → permanent（多次召回且
重要）或 → archived（衰减/久不用）的晋级/归档；archived 不参与召回。`memory cleanup`
显式触发衰减（非 permanent 记忆 importance 按 `decay_rate` 递减，<0.1 归档；
permanent 豁免）并删除超期 archived（距最后召回 > `archive_ttl_days`）；`memory stats`
显示 stage 分布与 archived 计数。检索排序在 bm25 之上融合生命周期因子（importance /
stage / recency，权重 `rank_lifecycle_weight`，=0 时与纯 bm25 等价）。记忆库带
`meta.schema_version`（当前 "1"），版本不符走迁移表不炸，未来版本库不回写版本号。

**语义检索（可选）**：`embedder = "remote"` 启用 OpenAI 兼容嵌入（协议对齐
`/v1/embeddings`；API key 从环境变量 `DEEPSEEKNOVA_EMBED_API_KEY` 读取，回落
`OPENAI_API_KEY`，不落配置/日志）。启用后写入记忆自动生成向量，召回按
`0.5*bm25 + 0.5*余弦 - rank_lifecycle_weight*生命周期惩罚` 融合排序，能找回换说法
但同义的记忆；缺 key、网络错或解析错一律 fail-open（warn 日志 + 回落纯 FTS）。
旧记忆用 `deepseeknova-cli memory embed-backfill` 显式回填（跳过 archived）；
`memory stats` 输出 `embedded=N/total=M` 显示覆盖率。

### Task Management

| Tool | Description | Read-only |
|---|---|---|
| `todo_write` | Create/update a structured task list | No |

### Skills

| Tool | Description | Read-only |
|---|---|---|
| `skill__<name>` | Activate a skill (one per registered skill) | Yes |

## Security Scan (CLI)

`deepseeknova-cli scan`（deepsec 式，P1）：内置正则 matcher 零 AI 定位候选点
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
deepseeknova-cli scan --format json --no-ai
deepseeknova-cli scan --path crates/deepseeknova-cli --severity-min high
```

## Eval (CLI)

`deepseeknova-cli eval` 用真实主 provider 逐条跑一个最小评估集，检查输出是否
包含要求的关键内容，适合在迭代后快速回归「还做得到」的能力：

```bash
deepseeknova-cli eval evals.jsonl            # 默认输出 Markdown
deepseeknova-cli eval --path evals.jsonl --format json
```

每条用例是一行 JSON（支持 `#` 注释与空行）：

```json
{"prompt": "用一句话解释 Rust 的 Ownership", "must_contain": ["所有权", "move"]}
```

| 参数 | 说明 |
|---|---|
| `--path <file>` | JSONL 文件路径，默认 `evals.jsonl` |
| `--format md\|json` | 报告格式，默认 `md` |

结果按用例逐一列出 pass/fail 与缺失子串；JSON 报告包含总览与逐条明细，方便
后续接入 CI 或脚本判定。

### 检查点（A1）

写类工具（write/edit/move）执行前自动快照，快照持久化在
`[checkpoint] path`（默认 `.deepseeknova/checkpoints.json`）：

```bash
deepseeknova-cli checkpoint list             # 查看快照与文件状态（unchanged/modified）
deepseeknova-cli checkpoint rollback         # 回滚最近一个快照
deepseeknova-cli checkpoint rollback --all   # 回滚全部
deepseeknova-cli checkpoint clear            # 丢弃快照（不恢复文件）
```

`[checkpoint] enabled = false` 可关闭。

### 会话效能度量（SessionMetrics）

每次 run 结束后，把执行面指标（步数 / 工具成败 / 重试 / 验证 / outcome）与成本面
（token 用量 + USD 估算）汇总成 JSON 报告，写入 `.deepseeknova/metrics/`
（默认开启）：

```toml
[metrics]
enabled = true    # 默认 true；false 时不采集、不落盘
```

报告文件名为 `<session_id>.json`，一个文件对应一次 run；写入失败仅记 warn，
不阻断 agent 运行。会话内实时成本仍用 `/cost` 查看，metrics 报告用于离线调优分析。

任务质量闭环在此基础上追加两类落盘产物（同目录）：

- `<session_id>.scorecard.json` — 六维评分卡（governance / verification /
  reflection / review / protocol / composite）+ overall，由 metrics hook 在
  run 结束时组装（findings 按 run 级差分切片，单会话上限 10000；protocol /
  composite 由 `[protocol]` 启用时经 `fill_protocol` 填充，见「协议增强能力包」节）。
- `diagnose/<session_id>.json` — 失败会话的结构化诊断报告（阶段时序 / 失败详情 /
  子代理链 / findings），落盘前脱敏（`redact_secrets`）+ Unix 0600 权限；成功或
  取消路径不产报告。

会话 id 三处同源：`<session_id>.json` 报告、`<session_id>.scorecard.json` 评分卡
与 `diagnose/<session_id>.json` 诊断使用同一标识。CLI 单次 run 标注为
`session-<ts>-<seq>`；serve 未显式标注时由 Agent 每次 run 生成唯一
`session-<ms>-<seq>`，避免多会话互相覆盖。serve 端点按该标识读取（见 HTTP API
节；`[metrics] enabled = false` 时诊断/评分卡均不落盘）。

### 项目后置产出（A2）

```bash
deepseeknova-cli artifacts wiki --project <name> --summary "<描述>"  # 生成 wiki/ 目录
deepseeknova-cli artifacts cards --title "..." --insight "..." --tags a --tags b  # 生成 cards/ 目录
```

### 代码图个性化（A3）

启用代码图后，repo map 会按当前用户输入提取标识符作为 personalized PageRank
seeds（去停用词、去重、上限 8），让地图优先展示任务相关模块。

### 代码图多跳查询（A3.1）

启用代码图后额外提供四个只读工具：

- `trace_code`：从任意符号出发沿 Calls/References/Dispatch 画多跳调用链
  （callers / callees / both，默认深度 6，超限输出标注 truncated）。
- `impact_code`：反向追踪谁会到达该符号，按文件聚合受影响符号与路径，
  用于重构爆炸半径估算。
- `explore_code`：一次传入多个符号，按文件分组输出带行号的源码片段
  （或 skeleton 签名视图）。
- `deps_code`：查询文件/符号的 import 依赖与依赖方（本地符号、文件间边、
  外部依赖标 `[external]`）；不带 entity 时输出全库外部依赖汇总。

### 符号引用与依赖图（A3.2）

- **References 边**：每个定义体引用的标识符按名称级解析到索引符号，回答
  「谁引用了 X」（`traverse_graph` 传 `edge_kinds=["references"]`）；同一对
  实体已有 Calls 边时不重复计，递归等 callee 不产生自引用。
- **结构化依赖图**：Rust `use`、Python `import/from`、JS/TS `import/require`、
  Go `import` 按语言解析——本地符号按名匹配成 文件→符号 Imports 边，JS 相对
  路径解析成文件→文件边，裸包名记入外部依赖（Go 相对路径 import 同样解析成
  文件→文件边）。
- **清单依赖**：`Cargo.toml`（dependencies/dev-dependencies/build-dependencies）、
  `package.json`（dependencies/devDependencies/peerDependencies/optionalDependencies）、
  `pyproject.toml`（[project] dependencies / [tool.poetry.dependencies]）、
  `go.mod`（require 段，含块式与单行）在 refresh 时解析进外部依赖表；
  `deps_code` 按文件归属最近清单展示。

代码图语言支持：Rust / Python / JavaScript / TypeScript / Go（tree-sitter 解析，
Go 覆盖函数/方法/struct/interface 实体、调用、import 三态：stdlib/第三方路径记
外部依赖、相对路径记本地文件）。

Rust trait 多态由「Dispatch 边」桥接：`impl Trait for Type` 中的同名方法会连到
trait 声明方法上，`dyn Trait` / 泛型调用点因此能列出全部候选实现（名称级匹配，
不做类型推断，同名方法可能多报候选）。

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
deepseeknova-cli serve --addr 127.0.0.1:3000
```

### ACP stdio 模式

`deepseeknova-cli serve --acp` 将进程切换为 Agent Client Protocol v1 的
stdio 服务器（换行分隔 JSON-RPC 2.0），供支持 ACP 的客户端直接拉起：

```bash
deepseeknova-cli serve --acp
```

支持的方法：`initialize`（协议版本协商 + 能力声明）、`session/new`（以请求的
`cwd` 作为工作区边界重建 agent）、`session/prompt`（流式输出
`agent_message_chunk` / `agent_thought_chunk` / 工具调用更新，结束返回
`stopReason`）、`session/cancel` 与 `session/close`。每个会话持有独立的多轮
历史，连续 prompt 会延续上下文。权限 `Ask` 因尚无 `session/request_permission`
RPC 而 fail-closed 拒绝；`mcpServers` 暂不连接（启动时告警并忽略）。

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

#### `GET /v1/runs`

列出持久化 run（新→旧）。服务启动时会把上次进程遗留的 `running` 任务标记为
`interrupted`。

```bash
curl http://localhost:3000/v1/runs
# [{"id":"<uuid>","prompt":"...","model":null,"created_at_ms":...,"status":"done",...}]
```

#### `POST /v1/runs/{id}/resume`

用保存的 prompt/model 重新执行一次 run，SSE 事件格式与 `/v1/chat` 相同。
`running` 状态返回 409；任务不存在返回 404。

```bash
curl -X POST http://localhost:3000/v1/runs/<id>/resume
```

#### `GET /v1/sessions/{id}/diagnose`

读取失败会话的结构化诊断报告（`diagnose/<id>.json` 落盘文件）。

```bash
curl http://localhost:3000/v1/sessions/session-1722830400-0/diagnose
# {"session_id":"...","outcome":"failed","phases":[...],"failures":[...],"sub_agents":[...],"quality":[...]}
```

文件不存在 → 404。仅限本机访问（服务默认监听 127.0.0.1），session id 走白名单
校验；无认证。

#### `GET /v1/sessions/{id}/scorecard`

读取单会话六维评分卡（`<id>.scorecard.json` 落盘文件；protocol/composite 为协议增强能力包新增维）。

```bash
curl http://localhost:3000/v1/sessions/session-1722830400-0/scorecard
# {"session_id":"...","dimensions":{"governance":1.0,"verification":0.8,"reflection":0.5,"review":1.0,"protocol":1.0,"composite":0.86},"overall":0.86}
```

文件不存在 → 404。

#### `GET /v1/metrics/scorecards`

扫描 `<metrics dir>/*.scorecard.json` 返回全部评分卡与聚合（均值/趋势/最差维度）。

```bash
curl http://localhost:3000/v1/metrics/scorecards
# {"scorecards":[...],"aggregate":{...}}
```

以上三端点由 CLI `serve` 自动接入工作区 `.deepseeknova/metrics/`（与运行时
落盘目录同一处），无需额外配置；`[metrics]` 段不提供 `dir` 字段。

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
deepseeknova-cli chat --tui
```

### Layout

```
┌─ 就绪 ───────────────────────────────────────────┐ ┌─ 侧边栏（Ctrl+\ 开合）─┐
│ 你: 看看 src/ 里有什么文件？                       │ │ 1 会话                 │
│ ⚙ ls ...                                          │ │ 2 工具活动             │
│   → src/main.rs, src/lib.rs                        │ │ 3 MCP                  │
│ [推理 ▸ 折叠 312 字符]  ← 推理默认折叠，Enter 展开   │ │ 4 成本                 │
│ 助手: src/ 目录包含 ...                             │ │ 5 技能                 │
│                                                    │ │                        │
├────────────────────────────────────────────────────┤ ├────────────────────────┤
│ model=deepseek-v4-flash  ready | turn 1 | ctx 19%  │ │ Tab/Ctrl+1..5 切面板    │
│   (12.3k / 64.0k) | $0.001234                      │ │ 窄终端(<90列)自动隐藏   │
├─ > prompt ─────────────────────────────────────────┤ │                        │
│ 第一行输入  @crates/…  ← @ 触发文件补全             │ │                        │
│ 第二行输入（Shift+Enter 换行）                      │ │                        │
│                                                    │ │                        │
└────────────────────────────────────────────────────┘ └────────────────────────┘
Ctrl+U 清行 · Ctrl+W 删词 · Shift+Enter 换行 · /help · Esc 取消/再按 Esc 退出
```

### 消息树与折叠

会话内容以**消息树**（Turn → Segment）实时增量构建：推理整段提交（不再被工具调用
从中间拆断），工具调用含参数与截断结果。推理与工具调用默认折叠显示摘要头
（`[推理 ▸ …]` / `[工具 ▸ …]`），`Enter` 展开查看参数与结果；成功结果截断、
失败醒目展开。

- `Tab` 循环焦点：输入 → 消息导航 → 侧边栏
- 消息导航焦点下：`j`/`k` 上移/下移选中消息，`Enter` 切换折叠，`y` 复制当前消息
- `/fold all` 折叠全部、`/fold none` 展开全部、`/fold reset` 恢复默认

### 命令面板

`Ctrl+K` 打开命令面板：模糊搜索全部命令（名称/描述/关键词），有参数的命令进入
内联参数输入后执行。与斜杠命令共用同一注册表；输入 `/` 时也会弹出命令模糊候选
（`↑`/`↓`/`j`/`k` 选择、`Enter` 执行、`Tab` 填入命令名），已输入参数
（如 `/fold `）时切换为枚举候选。候选超过 8 条时列表跟随选中项滚动，
高亮不会“滚出视野”。

侧边栏“会话”面板列出磁盘保存的会话（最新优先，含“当前”标记）：
`↑`/`↓`（或 `j`/`k`）选择，`Enter` 恢复选中会话；恢复内容进入对话面板
（可滚动/折叠），不再是临时回显。`/new` 后列表自动刷新。

首次启动（尚无对话）显示欢迎卡片：命令提示、快捷键说明与最近会话数；
提交第一个问题后自动消失。等待 agent 回复时，对话面板的 agent 位置
显示转圈动画（`⠋ 正在思考…`），首批内容到达后自动替换。

工具调用需要权限审批时，顶部弹出确认浮层：`y` / `Enter` 允许，`n` / `Esc` 拒绝
（拒绝结果回填 agent，行为与 deny 一致）。

### 配色与主题

配色走「新星观测台」语义表（`crates/deepseeknova-tui/src/theme.rs` 是唯一来源），
与桌面端同一张 token 表：

- accent / 用户 / 主动作：品牌蓝 `#4D6BFE`（deepseek 默认档）
- agent / 模型语声：柔蓝紫 `#7A8CFF`
- 推理、工具调用与结果、系统信息：dim（次要信息）
- 验证通过：green；验证失败 / 错误：red（错误加粗）
- 中断 / 标注：amber/yellow（预算条 >80%）
- diff 输出行级高亮：`+` 新增=green、`-` 删除=red、`@@` 块头=accent

主题经环境变量切换（默认 `deepseek`；`codex` 为兼容别名，等价默认档）：

```bash
DEEPSEEKNOVA_THEME=dark   deepseeknova-cli chat --tui   # 深色强调版（accent 亮化）
DEEPSEEKNOVA_THEME=light  deepseeknova-cli chat --tui   # 印刷星图浅色档（墨线 + 深化品牌蓝）
```

未知值回退 `deepseek` 并在会话内提示。

### 上下文占用

状态行显示 `ctx N% (used / window)`：分子为**最近一次请求的实际 tokens**
（从 provider usage 每帧刷新），分母为主模型配置的 `context_window`。未配置
`context_window` 时该段不显示。占用率 >80% 黄色警示、>95% 红色警示。

### 输入区增强

- 多行编辑 + 可见光标（←/→/Home/End、Shift+Enter 换行、Ctrl+U/W）
- markdown 行级着色（标题/列表/引用/代码围栏，仅影响显示）
- `@` 触发文件补全（候选清单由 CLI 注入工作区文件；↑↓ 选择、Enter 插入、Esc 关闭）
- 键位可经 `keybindings.json` 自定义（启动加载、改动热重载）；
  `Ctrl+X Ctrl+E` 用 `$EDITOR` 编辑当前输入（无 `$EDITOR` 时回退 vim）

### 按键

| Key | Action |
|---|---|
| `Enter` | 提交输入 |
| `Shift+Enter` / `Ctrl+J` | 输入内换行（多行输入） |
| `Tab` | 焦点循环：输入 → 消息导航 → 侧边栏 |
| `Esc` | 生成中取消；空闲时首次提示“再按 Esc 退出”（3 秒内再按退出）/ 关闭模态面板 |
| `Ctrl+C` | 生成中取消（空闲无操作） |
| `Ctrl+X Ctrl+E` | 用 `$EDITOR` 外部编辑当前输入 |
| `Ctrl+K` | 命令面板 |
| `Ctrl+\` | 侧边栏开合 |
| `↑` / `↓` | 输入历史（多行输入时移动光标行） |
| `←` / `→` | 输入内移动光标 |
| `Home` / `End` | 空闲=输入光标到行首/行尾；运行中=滚动到顶/跟随 |
| `Backspace` / `Delete` | 删除光标前/后字符 |
| `Ctrl+U` / `Ctrl+W` | 清空输入 / 删前一词 |
| `PageUp` / `PageDown` | 对话面板滚动回看 |
| `鼠标滚轮` | 滚动对话历史（滚到最新后自动恢复跟随） |
| `j` / `k` | 消息导航焦点：上下移动选中消息 |
| `Enter`（消息焦点） | 切换当前消息折叠 |
| `y`（消息焦点） | 复制当前消息（剪贴板不可用时降级回显） |
| `↑`/`↓`（侧边栏会话面板） | 选择保存的会话 |
| `Enter`（侧边栏会话面板） | 恢复选中会话 |

### 斜杠命令

| 命令 | 作用 |
|---|---|
| `/help` | 显示帮助 |
| `/clear` | 清空对话面板 |
| `/new` | 开始新会话（更换 session id） |
| `/sessions` | 列出已保存会话 |
| `/resume <id>` | 恢复指定会话并渲染进对话面板 |
| `/model` | 显示模型与指针；`/model effort <level>`、`/model thinking`、`/model switch <name>`、`/model use <role> <name>` |
| `/cost` | 按模型×角色输出 token 用量与美元估算 |
| `/scorecard` | 读取 `.deepseeknova/metrics` 最新评分卡，输出六维测光表 |
| `/skills` | 列出 `.deepseeknova/skills` 与 `.agents/skills` 中的技能 |
| `/mcp` | 列出已启用 MCP server 并实时探测连接状态（✓ 已连接 / ✗ 未连接） |
| `/raw` | 切换显示模式 normal / lite / raw（lite 隐藏推理，raw 带类型前缀） |
| `/fold` | 折叠控制：`/fold all`、`/fold none`、`/fold reset` |
| `/copy` | 复制当前选中消息 |
| `/undo` | 回滚最近一个检查点快照 |
| `/undo all` | 回滚全部快照 |
| `/undo list` | 列出快照与 ✓/✗ 状态 |
| `/quit` | 退出 TUI |

## System Prompts

主 agent 内置一套英文默认系统提示词（`deepseeknova_agent::DEFAULT_SYSTEM_PROMPT`），
核心设计：把 DeepSeek-V4-Flash 当作**低成本高频决策引擎**，而不是一次性回答机器；
所有任务按显式循环执行：**Observe → Plan → Tool → Verify → Reflect → Next Action**；
每轮一个动作、先工具后长文、能查不猜、完成前必须验证与反思、成本敏感。

- 默认启用：`[agent]` 未配置 `system_prompt` 时自动注入内置默认提示词。
- 覆盖：配置 `system_prompt = "..."` 即完全替换默认值。
- 追加：运行时（如启用代码图）会在默认/自定义提示词后追加英文检索策略提示。
- 全链路统一：规划器（plan_mode / coordinator）、子代理预设（explorer / coder /
  tester / reviewer）、审查（review）、压缩（compaction）、安全调查（scanner）、
  观察压缩与验证回炉文案均与六阶段循环术语一致；机器输出契约
  （JSON 结构、章节名、工具清单）保持不变。
- 设计文档：`PROMPT_DESIGN.md`；后端完整性报告：`BACKEND_AUDIT.md`。

## MCP Integration

deepseeknova can connect to MCP (Model Context Protocol) servers for additional tools.

### Configuration

```toml
mcp_servers = [
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
deepseeknova-cli run "Refactor the auth module to use JWT"
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
deepseeknova-cli run "Audit the entire codebase for security issues"
```

The coordinator agent spawns sub-agents for independent work (e.g., one per module) and
synthesizes their results.

## Sandbox

When `[sandbox] enabled = true`, shell commands run in an OS-level sandbox
(runtime 按平台选择三档中的 `WorkspaceWrite`：工作区默认可写，网络按
`allow_network` 配置，默认禁网):

- **macOS**: Seatbelt (Apple Sandbox)
- **Linux**: bubblewrap (bwrap)
- **Windows**: `NoOpSandbox`（无隔离，待实现 Job Object / AppContainer）

Read-only tools (`read_file`, `grep`, `glob`, `ls`) are unaffected by sandbox settings.

## Advanced Configuration

### Custom Provider

```toml
[[providers]]
name = "anthropic"            # 被 [[models]]/plan_mode 引用的唯一名
kind = "anthropic"
base_url = "https://api.anthropic.com/v1"
model = "claude-sonnet-5"
api_key_env = "ANTHROPIC_API_KEY"
max_tokens = 8192
```

### Copilot Provider

```toml
[[providers]]
name = "copilot"
kind = "openai"
base_url = "https://api.githubcopilot.com"
model = "gpt-4o"
api_key_env = "GITHUB_TOKEN"
```

### Multiple Providers

```toml
[[providers]]
name = "openai"
kind = "openai"
model = "gpt-4o"

[[providers]]
name = "anthropic"
kind = "anthropic"
model = "claude-sonnet-5"

# Use Anthropic for specific skills or plan mode
[plan_mode]
provider = "anthropic"        # 引用 [[providers]] 里的 name
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
enabled = true                  # 总开关：false（默认）时工具不经过 allow/ask/deny 门控
default_mode = "ask"            # 无规则命中时写工具的默认行为：ask | allow | deny
# rate_limit_per_minute = 30    # 可选：滚动 60s 窗口内的门控调用上限

# 规则按顺序匹配，deny > ask > allow > default_mode 优先级（subject 可省略）
[[permissions.rules]]
tool = "bash"
mode = "ask"

[[permissions.rules]]
tool = "bash"
subject = "rm *"
mode = "deny"

[[permissions.rules]]
tool = "read_file"
subject = "*.env"
mode = "deny"
```

普通 shell 组合（链式/重定向/命令替换等）按非只读走权限审批/规则，可由 allow 规则覆盖；工具级注入面（`git -c`/`--config-env`、格式串注入、UNC/URL 路径形态等）直接硬拒，不可通过规则覆盖。
