# deepseeknova User Guide

## Table of Contents

1. [Concepts](#concepts)
2. [Installation & Setup](#installation--setup)
3. [Configuration](#configuration)
4. [Tools Reference](#tools-reference)
5. [Security Scan (CLI)](#security-scan-cli)
6. [Exec Audit (CLI)](#exec-audit-cli)
7. [Eval (CLI)](#eval-cli)
8. [Skills](#skills)
9. [HTTP API](#http-api)
10. [TUI](#tui)
11. [MCP Integration](#mcp-integration)
12. [Plan Mode](#plan-mode)
13. [Sub-Agents](#sub-agents)
14. [Sandbox](#sandbox)
15. [Worktrees (CLI)](#worktrees-cli)
16. [Advanced Configuration](#advanced-configuration)

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
    async fn execute(
        &self,
        ctx: &ToolContext,
        args: &str,
    ) -> Result<String, deepseeknova_core::DeepseeknovaError>;
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

- Rust 1.97 or later (`rustup update stable`)
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
├── AGENTS.md          # 项目级 Agent 指令模板（行业标准文件名，Claude Code / Codex / opencode / DeepseekNova 自动识别）
├── deepseeknova.toml  # 项目配置（Config::load 项目层）
└── .deepseeknova/
    ├── commands/      # 自定义斜杠命令（含 build.md 示例）
    └── memory/        # 记忆库目录
```

默认生成行业标准的 `AGENTS.md`，内含项目简介 / 常用命令 / 代码约定骨架，
供各类 AI 编程工具读取。若 `AGENTS.md` 已存在则跳过并提示。

如需向后兼容的私有文件名，可回退到 `DEEPSEEKNOVA.md`：

```bash
deepseeknova-cli init --legacy   # 生成 DEEPSEEKNOVA.md
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
# network_allow_domains = ["api.github.com"]   # 域名白名单（warn-not-fail 校验；
                                   # seatbelt/bwrap 当前仅支持整网开关，域名级过滤为后续）

[permissions]
enabled = true                     # true（默认，默认安全姿态）：工具经 allow/ask/deny 门控
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

### 会话级花费上限（`[budget]`）

Token 预算之上可再加 **USD 花费上限**：`max_total_cost_usd` 由共享
`CostLedger` 按 `[[models]]` 单价估算会话累计花费，超限即以
`Paused(budget: cost limit ...)` 结束（保留 `budget:` 前缀对齐 CLI 退出码）；
与 `max_total_tokens` 并列，任一先到先停。无单价可估时该上限退化为不生效
（fail-open on unknown cost）：

```toml
[budget]
enabled = true                 # 步边界预算守卫（默认 true）
max_total_tokens = 128000      # 上下文 token 硬上限（默认值）
max_total_cost_usd = 0.50      # 可选：会话级花费上限（None = 不限；拒绝负数/NaN/inf）
```

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
allow_recursion = false            # 允许子代理再派子代理（coordinator 子代理递归）
max_depth = 3                      # 递归深度上限（allow_recursion=true 时生效）

[[delegate.agents]]
name = "coder"
max_steps = 25
tools = ["read_file", "write_file", "edit_file", "bash"]

[[delegate.agents.inputs]]
name = "path"
value = "src/lib.rs"
```

> `allow_recursion = true` 开启后，coordinator 子代理可再派子代理，深度受
> `max_depth` 约束、超深优雅降级。主 agent 的 delegate 工具路径递归仍待
> 后续轮（深度传播需 Agent 主循环注入）。

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

### 用户级 hooks（`[hooks]`）

把事件接到外部命令（区别于上文的内部 ToolHook 链）：五事件
`tool_before` / `tool_after` / `session_start` / `session_end` / `failure`，
事件间 **AND 链**——`tool_before` 的全部命令通过（exit 0 且裁决放行）才执行
工具，任一失败（非 0 / 超时 / 崩溃 / `allowed=false`）即阻止（**fail-closed**，
叠加在内部治理链之后）；`tool_after` / `session_*` / `failure` 失败仅 warn。
无 hooks 配置零进程开销：

```toml
[hooks]
enabled = true                            # 总开关（默认 false → 不装配）
tool_before = [
  { command = "scripts/guard.sh", args = ["--check"], timeout_secs = 10 },
]
```

JSON 协议：stdin 传 `{"event","tool","arguments","workspace","session_id"}`，
stdout 期望 `{"allowed":bool,"reason"}`。`session_start`/`session_end` 于 run
边界触发，`failure` 挂 MetricsGuard emit（Paused/异常触发，Completed/Cancelled
不触发）。单条命令可配 `disabled = true` 保留配置但跳过执行。

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
| `DEEPSEEKNOVA_LANG` | TUI 界面语言（`zh`/`zh-cn`/`cn`/`chinese`/`中文` → 中文） |
| `DEEPSEEKNOVA_KEYBINDINGS` | TUI 键位定制文件路径（默认 `~/.deepseeknova/keybindings.json`） |

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

**代码图语义检索（可选）**：同一 `embedder = "remote"` 生效后，代码图索引
（`search_code` 等图工具）自动启用语义检索（写入即嵌入 + `w*bm25 +
(1-w)*余弦` 融合，默认 w=0.5）；缺 key/网络错 fail-open 回落纯 FTS。旧索引
无需重建（SCHEMA 增量加向量表，打开即迁移）。

### 界面语言（`[ui]`）

```toml
[ui]
lang = "en"   # en（默认）| zh（接受 zh-cn/cn/chinese/中文 别名）
```

`lang` 缺省回退 `DEEPSEEKNOVA_LANG` 环境变量（`zh`/`zh-cn`/`cn`/`chinese`/`中文` →
中文，其余/缺省 → 英文），两者皆缺省为英文。词表结构见
`crates/deepseeknova-tui/src/i18n/`。

**记忆用户面（CLI）**：除 agent 的 `remember`/`recall`/`forget` 工具外，可用
`deepseeknova-cli memory` 直接浏览与管理记忆库：

```text
memory list    [--category task|skill|user_profile|all] [--stage candidate|verified|
                permanent|archived] [--tag <tag>] [--search <kw>] [--limit N] [--offset N]
                分页列出记忆（id/类目/stage/importance/recall_count/最近召回时间 +
                内容摘要）；`--stage` 按生命周期阶段、`--tag` 按标签精确匹配、
                `--search` 按内容子串过滤。
memory edit    <id> <content...>   改写内容（保留 id/tags/source 与 lifecycle 元数据；
                                    启用嵌入时强制重算向量，旧向量不残留）。
memory delete  <id> [--yes]         删除记忆，**二次确认**（不可逆）；--yes 跳过确认。
memory replay  <query> [--top-k N]  召回回放：执行一次与 recall 完全同源的混合检索，
                                    展示每条命中的 id/内容与分数分解
                                    （score = bm25 + cosine + lifecycle，mode=hybrid|fts），
                                    让你看到"为什么召回这条"；回放是只读诊断，
                                    不记召回命中率、不晋级 lifecycle。
memory search  <query>              按相关度检索（同 recall 路径，会记召回/晋级）。
memory forget  <id>                 按 id 直接删除（无确认，脚本用）。
memory stats / embed-backfill / cleanup   统计 / 嵌入回填 / 衰减清理。
```

编辑/删除按内容摘要即可定位；`memory list --search` 与 `--tag` 可组合缩小范围。

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

## Exec Audit (CLI)

`deepseeknova-cli audit` 对一条 shell 命令（或「工具名 + JSON 参数」）做
**预执行**安全决策预览——只计算不执行：输出只读放行 / Ask / 硬拒 + 命中规则 +
只读分类形态 + 建议，与真实 permission gate 共用同一 preflight 代码路径
（一致性有测试背书），适合在接入 agent 前人工核对命令会被如何判定：

```bash
deepseeknova-cli audit "git status"                  # 只读放行
deepseeknova-cli audit "rm -rf /tmp/x"               # Ask / 规则命中
deepseeknova-cli audit bash '{"command":"git status"}'
deepseeknova-cli audit --format json --rules         # 导出当前规则表（md/json）
```

| 参数 | 说明 |
|---|---|
| `--format md\|json` | 报表格式，默认 `md` |
| `--rules` | 只导出规则表（不审计具体命令，此时可省略命令） |
| `--workspace <dir>` | 工作区根（路径守卫按此判定），缺省当前目录 |

## Eval (CLI)

`deepseeknova-cli eval` 用真实主 provider 逐条跑一个评估集，并按**分级评判**
（评分卡综合分 / 单维 / 成本 / 子串）产出通过、失败与各断言明细，适合迭代后
回归「还做得到」的能力，也可接入 CI 作为质量门槛：

```bash
deepseeknova-cli eval evals.jsonl            # 默认输出 Markdown
deepseeknova-cli eval --path evals.jsonl --format json
deepseeknova-cli eval --require-min-score 3.5 \
  --require-dimension governance>=0.9 --require-dimension 协议>=0.8   # CI 门禁
```

每条用例是一行 JSON（支持 `#` 注释与空行）；**同一用例的多个断言 AND 语义**，
全部满足才 pass：

```json
{"prompt": "用一句话解释 Rust 的 Ownership", "must_contain": ["所有权", "move"]}
{"prompt": "修复一个越权访问漏洞", "min_score": 0.8,
 "dimension_min": {"governance": 0.9, "协议": 0.8}, "cost_max": 0.05, "rounds": 3,
 "name": "越权修复"}
```

| 断言字段 | 说明 |
|---|---|
| `must_contain` | 输出包含给定子串（保持兼容） |
| `min_score` | 会话评分卡综合分下限。0..5 分制；`<= 1.0` 时按 0..1 折算（等价 `×5`），即 `0.8` 与 `4.0` 等价 |
| `dimension_min.<name>` | 评分卡单维（0..1）下限；name 支持英文名 `governance/verification/reflection/review/protocol/composite` 与中文别名 `治理/验证/反思/审查/协议/综合` |
| `cost_max` | 本用例全部轮次累计 token 成本（USD）上限 |
| `rounds` | 重试轮次上限（默认 1 = 单轮；0 视为 1）。任一轮全部断言通过即停，报告记录实际轮次 |
| `name` | 用例名（报告可读性；缺省用 `case N`） |

> 成本单位为 USD（与 `[model_pointers]` 单价表口径一致）；未配置单价或调用
> 未计量时 `cost_max` 断言判失败并注明「成本不可用」，不伪造数值。

| 参数 | 说明 |
|---|---|
| `--path <file>` | JSONL 文件路径，默认 `evals.jsonl` |
| `--format md\|json` | 报告格式，默认 `md` |
| `--require-min-score <N>` | CI 门槛：全部用例综合分均值（0..5）下限 |
| `--require-dimension <name>=<N>` | CI 门槛：单维均值（0..1）下限，可重复（name 支持英文名与中文别名） |

结果按用例逐一列出 pass/fail 与各断言明细（实际值 vs 阈值）、综合分（0..5）、
六维分数、成本与轮次；Markdown 报告末尾附 CI 门槛检查。进程退出码区分
「条目级失败」与「CI 门槛失败」，供 CI 门禁：

| 退出码 | 含义 |
|---|---|
| `0` | 全部用例通过且 CI 门槛满足 |
| `1` | 仅条目级失败（有用例未通过，CI 门槛满足） |
| `2` | 仅 CI 门槛失败（用例全过但均值未达门槛） |
| `3` | 两者皆有 |

> eval 的评分卡在内存中捕获，不写入 `.deepseeknova/metrics/`，避免污染
> `GET /v1/metrics/scorecards` 的质量驾驶舱聚合；质量钩子（quality /
> diagnose / protocol 门控）随 build_agent 照常生效。

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
# 可选：保护所有 /v1/* 路由（除 /health 探活外均需 Bearer token）
deepseeknova-cli serve --addr 127.0.0.1:3000 --token <your-token>
```

> **认证**：配置 `--token` 后，所有 `/v1/*` 请求须携带
> `Authorization: Bearer <token>`，否则返回 401；`/health` 保持免认证以便
> 探活。默认（无 token）服务开放于 127.0.0.1，仅限可信本机使用。
>
> **CORS**：跨源浏览器请求仅放行 loopback 来源（`localhost` / `127.0.0.1` /
> `::1`，端口不限）；其他 Origin 的响应不带 `Access-Control-Allow-Origin`，
> 浏览器会拒绝读取——恶意网页无法跨源读取 SSE/会话/评分卡或自答
> `/v1/approval`。无 `Origin` 头的请求（curl、非浏览器客户端）不受影响。

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
data: {"text":"...","tool_calls":[...],"usage":{...},"session_id":"<run 或 session id>"}
```

`done` 事件携带 `session_id` 关联键：`/v1/sessions/{id}/chat` 为该会话 id，
`/v1/chat` 与 `/v1/runs/{id}/resume` 为 durable run id——前端可据此拉取该 run
的评分卡（`GET /v1/sessions/{id}/scorecard`）。未配置持久化时字段为 `null`。

**SSE 事件清单**（`POST /v1/chat`、`POST /v1/runs/{id}/resume`、
`POST /v1/sessions/{id}/chat` 共用同一事件集）：

| event | data 内容 |
|-------|-----------|
| `text` | 增量正文（逐 token 追加渲染） |
| `reasoning` | 推理增量文本 |
| `tool_start` | `{"id","name"}` 工具调用开始 |
| `tool_end` | `{"id","name","arguments"}` 工具调用结束（含累计 arguments） |
| `tool_result` | `{"call_id","result"}` 工具执行结果 |
| `usage` | token 用量（prompt/completion/total/cache/reasoning 等） |
| `done` | `{"text","tool_calls","usage","session_id"}` run 结束 |
| `approval_request` | `{"id","title","description"}` 权限 Ask（需 `POST /v1/approval` 应答） |
| `paused` | `{"reason","session_id"}` run 暂停（可恢复） |
| `verification` | `{"command","passed","summary"}` P4 验证命令结果 |
| `quality_finding` | 任务质量闭环 finding |
| `phase_transition` | 阶段迁移事件 |
| `gate_violation` | 门控违规记录 |
| `drift_finding` | drift 检测结果 |
| `error` | 流错误 / 校验失败（prompt 为空、超长等） |

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

#### `GET /v1/sessions` / `POST /v1/sessions`

会话级 HTTP 接口（与 TUI/CLI 共用同一 JSONL store，默认 `~/.deepseeknova/sessions`，
跨端看到同一批会话）。`GET` 列出会话摘要（新→旧），`POST` 创建一个空会话并返回
其 id：

```bash
curl http://localhost:3000/v1/sessions
# [{"id":"session-...","turns":0,"updated_at_ms":...}]
# 有回合后 title 为首回合 prompt 截断：{"id":"...","turns":1,"updated_at_ms":...,"title":"hello session"}

curl -X POST http://localhost:3000/v1/sessions
# {"id":"session-1722830400-123"}
```

#### `GET /v1/sessions/{id}` / `DELETE /v1/sessions/{id}`

`GET` 返回某会话已存回合（旧→新，`StoredTurn` 数组）；`DELETE` 删除会话，正在
执行中的会话返回 409，不存在的返回 404：

```bash
curl http://localhost:3000/v1/sessions/session-1722830400-123
# [{"turn":1,"timestamp":"2026-08-07T...","input":{"prompt":"..."},"output":{"text":"..."},"messages":[...]}]

curl -X DELETE http://localhost:3000/v1/sessions/session-1722830400-123
# {"deleted":true}
```

#### `POST /v1/sessions/{id}/chat`

在会话内跑一回合 prompt，SSE 事件集与 `/v1/chat` 相同，但 runner 绑定该会话的
共享多轮历史（连续 prompt 延续上下文）；`done` 事件的 `session_id` 即该会话 id。
回合完成后落盘（仅用户 prompt + 助手最终正文，口径与 TUI 一致）。同一会话并发
prompt 返回 409：

```bash
curl -X POST http://localhost:3000/v1/sessions/session-1722830400-123/chat \
  -H 'Content-Type: application/json' \
  -d '{"prompt":"继续刚才的话题"}'
```

#### `GET /v1/sessions/{id}/diagnose`

读取失败会话的结构化诊断报告（`diagnose/<id>.json` 落盘文件）。

```bash
curl http://localhost:3000/v1/sessions/session-1722830400-0/diagnose
# {"session_id":"...","outcome":"failed","phases":[...],"failures":[...],"sub_agents":[...],"quality":[...]}
```

文件不存在 → 404。配置 `--token` 后受保护；默认（无 token）仅限本机访问
（服务默认监听 127.0.0.1），session id 走白名单校验。

#### `GET /v1/sessions/{id}/scorecard`

读取单会话六维评分卡（`<id>.scorecard.json` 落盘文件；protocol/composite 为协议增强能力包新增维；`overall` 为派生计算值、不入库，以 `composite` 综合指数与各维均值参考）。

```bash
curl http://localhost:3000/v1/sessions/session-1722830400-0/scorecard
# {"session_id":"session-1722830400-0","started_at_ms":...,"dimensions":{"governance":1.0,"verification":0.8,"reflection":0.5,"review":1.0,"protocol":1.0,"composite":0.86},"first_pass":true,"retry_rounds":0}
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
┌─ 对话区（无边框，Claude Code 风格）──────────────┐ ┌─ 侧边栏（Ctrl+\ 开合）─┐
│ ❯ 看看 src/ 里有什么文件？                        │ │ 1 会话                 │
│                                                    │ │ 2 工具活动             │
│ ⏺ Bash(ls src/)                                    │ │ 3 MCP                  │
│   ⎿  src/main.rs, src/lib.rs   ← 工具默认展开      │ │ 4 成本                 │
│ [推理 ▸ 折叠 312 字符]  ← 推理默认折叠，Enter 展开   │ │ 5 技能                 │
│ ⏺ src/ 目录包含 ...                                │ │                        │
│ ⠸ 思考…（3s · Ctrl+C 取消）  ← 等待动画+耗时       │ │                        │
├────────────────────────────────────────────────────┤ ├────────────────────────┤
│ ● deepseek-v4-flash·high │ ctx [██░░] 19% │ 权限 default │ ⎇ main │ Tab 切面板 · 1..5 直接切
├─ ❯ prompt ─────────────────────────────────────────┤ │ 窄终端(<90列)自动隐藏   │
│ 第一行输入  @crates/…  ← @ 触发文件补全             │ │                        │
│ 第二行输入（Shift+Enter 换行）                      │ │                        │
└────────────────────────────────────────────────────┘ └────────────────────────┘
Ctrl+U 清行 · Ctrl+W 删词 · Shift+Enter 换行 · /help · Esc 取消/再按 Esc 退出
```

### 消息树与折叠

会话内容以**消息树**（Turn → Segment）实时增量构建：推理整段提交（不再被工具调用
从中间拆断），工具调用含参数与截断结果。渲染为 Claude Code 风格：`❯` 标用户输入、
`⏺` 标 agent 输出与工具调用（圆点颜色编码状态：运行中 dim / 成功 accent / 失败红）、
`  ⎿  ` 缩进展示工具结果。**推理默认折叠**显示摘要头（`[推理 ▸ …]`），**工具调用
默认展开**（参数与截断结果直接可见）；`Enter` 切换单个段的折叠态，成功结果截断、
失败醒目。成本/turn/usage 等明细不在状态行，用 `/cost` 等命令查看。

- `Tab` 循环焦点：输入 → 消息导航 → 侧边栏
- 消息导航焦点下：`j`/`k` 上移/下移选中消息，`Enter` 切换折叠，`y` 复制当前消息
- `/fold all` 折叠全部、`/fold none` 展开全部、`/fold reset` 恢复默认

### 命令面板

输入 `/` 打开命令面板：模糊搜索全部命令（名称/描述/关键词），有参数的命令进入
内联参数输入后执行。与斜杠命令共用同一注册表（无 `Ctrl+K` 绑定——命令面板是
纯 `/` 触发，见 `crates/deepseeknova-tui/src/app/actions.rs` 的说明）；输入 `/`
时弹出命令模糊候选（`↑`/`↓`/`j`/`k` 选择、`Enter` 执行、`Tab` 填入命令名），
已输入参数（如 `/fold `）时切换为枚举候选。候选超过 8 条时列表跟随选中项滚动，
高亮不会“滚出视野”。

侧边栏“会话”面板列出磁盘保存的会话（最新优先，含“当前”标记）：
`↑`/`↓`（或 `j`/`k`）选择，`Enter` 恢复选中会话；恢复内容进入对话面板
（可滚动/折叠），不再是临时回显。`/new` 后列表自动刷新。

每个回合落盘时记录**工作区根路径**（`StoredTurn.workspace`，旧会话文件向后兼容
读为全局），会话面板按工作区分组显示（`⎇ {项目名} · {会话数}`，未知/旧会话归
`全局` 组），组内再按夜次分组。`/workspace` 输出每工作区会话数明细；
`/sessions` 对非当前工作区的会话标注 `[{项目名}]`。跨项目切换后，侧边栏仍能
一眼找到各项目的历史会话（会话文件本身继续存在 `~/.deepseeknova/sessions`，
按项目隔离会话仍用 `deepseeknova-cli worktree new`）。

其余面板均为实时数据，不再是占位提示：**MCP** 面板进入即异步探测（每 30s
冷却刷新），逐 server 显示 `✓ 已连接 / ✗ 未连接（原因）`；**技能**面板启动时
一次性扫描 `.deepseeknova/skills` 与 `.agents/skills` 并列出 `name — description`；
**工具活动**聚合统计；**成本**显示会话累计与测光评分卡。

首次启动（尚无对话）显示简洁欢迎区（Claude Code 风格，无边框卡片）：
logo、命令提示、快捷键说明、最近会话数与当前工作目录；提交第一个问题后
自动消失。等待 agent 回复时，对话面板的 agent 位置显示转圈动画 +
随机动词 + 已耗时间（`⠸ 思考…（3s · Ctrl+C 取消）`），首批内容到达后自动替换。

工具调用需要权限审批时，顶部弹出确认浮层：`y` / `Enter` 允许，`n` / `Esc` 拒绝
（拒绝结果回填 agent，行为与 deny 一致）。

### 配色与主题

配色走「新星观测台」语义表（`crates/deepseeknova-tui/src/theme.rs` 是唯一来源），
单一 token 表。整体观感对齐 Claude Code：低饱和、dim 为主——

- 用户 / agent 正文：终端默认前景色（归属靠 `❯` / `⏺` 标记区分，不整行染色）
- accent / 主动作（`❯` 提示符、`⏺` 标记、模型标签）：品牌蓝 `#4D6BFE`（deepseek 默认档）
- 推理、工具参数与结果、系统信息：dim（次要信息）
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
- 键位可经 `keybindings.json` 自定义（启动加载、改动热重载）；文件路径
  `~/.deepseeknova/keybindings.json`，可用 `DEEPSEEKNOVA_KEYBINDINGS` 环境变量
  覆盖。格式与 Claude Code 同构：`{"bindings":[{"context":"Input","bindings":
  {"ctrl+u":"conv:scrollTop","y":null}}]}`，`null` 解绑默认键。action 名见
  `/help` 与 `crates/deepseeknova-tui/src/app/actions.rs`；`ctrl+c/ctrl+d/ctrl+m/
  ctrl+z/ctrl+x` 为保留键（app 占用，见该文件 `reserved_reason`）。Input 编辑键
  （Enter/Shift+Enter/Ctrl+A/E/U/W/Tab/方向键/Home/End 等）、Conversation/
  Sidebar/Completion 焦点与全局热键（Ctrl+P、Ctrl+\、F1、Ctrl+L）均受 keymap
  覆盖——改键或解绑即时生效。
  `Ctrl+X Ctrl+E` 用 `$EDITOR` 编辑当前输入（无 `$EDITOR` 时回退 vim）

### 按键

| Key | Action |
|---|---|
| `Enter` | 提交输入 |
| `Shift+Enter` / `Ctrl+Enter` | 输入内换行（多行输入） |
| `Tab` | 焦点前进：输入 → 消息导航 → 侧边栏 |
| `Esc` | 生成中取消；空闲时首次提示“再按 Esc 退出”（3 秒内再按退出）/ 关闭模态面板 |
| `Ctrl+C` | 生成中取消；空闲时与 Esc 同语义（再按退出） |
| `Ctrl+D` | 空输入时退出（shell 惯例；raw 模式下由应用桥接） |
| `Ctrl+Z` | 提示 TUI 下无法挂起进程（raw 模式无任务控制） |
| `Ctrl+X Ctrl+E` | 用 `$EDITOR` 外部编辑当前输入 |
| `/` | 命令面板（纯 `/` 触发，无 Ctrl+K） |
| `Ctrl+\` | 侧边栏开合 |
| `Ctrl+P` | 循环切换权限模式（plan → accept_edits → auto） |
| `Ctrl+T` | 切换鼠标捕获（滚轮滚动对话 vs 鼠标选中复制） |
| `F1` | 打开 `/help` 帮助浮层 |
| `Ctrl+L` | 清屏重绘 |
| `↑` / `↓` | 输入历史（多行输入时移动光标行） |
| `←` / `→` | 输入内移动光标 |
| `Home` / `End` | 空闲=输入光标到行首/行尾；运行中=滚动到顶/跟随 |
| `Backspace` / `Delete` | 删除光标前/后字符 |
| `Ctrl+A` / `Ctrl+E` | 输入光标到行首/行尾 |
| `Ctrl+U` / `Ctrl+W` | 清空输入 / 删前一词 |
| `PageUp` / `PageDown` | 对话面板滚动回看 |
| `鼠标滚轮` | 滚动对话历史（滚到最新后自动恢复跟随） |
| `Ctrl+4` | 与 `Ctrl+\` 等价（终端同一字节，跨平台兜底） |
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
| `/rename <title>` | 为当前会话命名（`titles.json` 落盘，无 title 回退 id） |
| `/model` | 显示模型与指针；`/model effort <level>`、`/model thinking`、`/model switch <name>`、`/model use <role> <name>` |
| `/cost` | 按模型×角色输出 token 用量与美元估算 |
| `/scorecard` | 读取 `.deepseeknova/metrics` 最新评分卡，输出六维测光表 |
| `/skills` | 列出 `.deepseeknova/skills` 与 `.agents/skills` 中的技能 |
| `/mcp` | 列出已启用 MCP server 并实时探测连接状态（✓ 已连接 / ✗ 未连接） |
| `/raw` | 切换显示模式 normal / lite / raw（lite 隐藏推理，raw 带类型前缀） |
| `/fold` | 折叠控制：`/fold all`、`/fold none`、`/fold reset` |
| `/copy` | 复制当前选中消息 |
| `/workspace` | 显示当前工作区（路径 + git 分支）、会话数、可用 git worktree 与切换/隔离提示 |
| `/undo` | 回滚最近一个检查点快照 |
| `/undo all` | 回滚全部快照 |
| `/undo list` | 列出快照与 ✓/✗ 状态 |
| `/checkpoint` | 会话级检查点：`save [label]` / `list` / `rollback [id]`（快照对话行 + 容量 FIFO + JSONL 持久化，回退同步重写模型上下文） |
| `/mode` | 权限模式循环/切换：`plan` / `accept_edits` / `auto` / `cycle`（写工具默认裁决强度） |
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
`allow_network` 配置，默认禁网；`network_allow_domains` 为域名白名单配置接口，
条目格式非法时 warn-not-fail——seatbelt/bwrap 当前仅支持整网开关，域名级过滤
需 DNS 解析后按 IP 过滤，属后续增强):

- **macOS**: Seatbelt (Apple Sandbox)
- **Linux**: bubblewrap (bwrap)
- **Windows**: `NoOpSandbox`（无隔离，待实现 Job Object / AppContainer）

Read-only tools (`read_file`, `grep`, `glob`, `ls`) are unaffected by sandbox settings.

## Worktrees (CLI)

`worktree` 子命令用 **git worktree** 提供隔离的并行会话：在同一仓库上并行跑
多个 agent 会话时，每个会话在一个独立工作副本中修改文件，互不干扰——适合
"Agent 框架"定位下的并行开发、多任务验收或 A/B 实验。

```bash
deepseeknova-cli worktree new [--name <name>] [--base <ref>]   # 创建隔离副本
deepseeknova-cli worktree list                                  # 列出全部 worktree
deepseeknova-cli worktree switch <name>                         # 打印目标目录（供 cd）
deepseeknova-cli worktree delete <name> [--force]               # 删除 worktree
deepseeknova-cli worktree clean                                 # 清理全部 CLI 创建的 worktree
```

### 创建与进入（new / switch）

`worktree new` 在主工作树根的 `.deepseeknova/worktrees/<name>` 下执行
`git worktree add -b <name> <path> HEAD`：`--name` 缺省生成 `wt-<时间戳>-<序号>`
（同时作为新分支名）；`--base <ref>` 指定基础 ref（分支 / tag / commit，缺省
`HEAD`）。创建成功后 CLI 打印进入指引：

```bash
✓ worktree `feat-x` created at /path/to/repo/.deepseeknova/worktrees/feat-x
  branch: feat-x (base: HEAD)

Start an isolated session inside it:
  cd /path/to/repo/.deepseeknova/worktrees/feat-x
  deepseeknova-cli chat --tui       # interactive (or `run "<task>"` for one-shot)
```

`worktree switch <name>` 打印目标目录供 `cd` 进入（CLI 无法改变父进程的工作目录）。
`worktree list` 列出全部 worktree（路径 / 分支 / `*` 当前标记 / `[cli]` 标记，
`[cli]` 表示由本 CLI 管理）。

### 会话隔离语义

每个 worktree 是一个**独立的工作副本**，其中启动的会话其运行时状态
（`graph.db` 代码图索引、`memory.db` 记忆库、`metrics/` 评分卡与诊断、
`checkpoints.json` 等）按**工作区根**落盘——即落在该 worktree 自己的
`.deepseeknova/` 下，天然互不干扰：

- 工作区根的 `.deepseeknova/` 已被仓库 `.gitignore` 的 `.deepseeknova/*` 覆盖，
  创建 worktree 不会污染主工作树 `git status`，会话状态也不会被误提交。
- 两个会话并行修改同名文件时，各改各的工作副本，互不覆盖。
- 与现有会话管理的关系：`chat --tui` 的 `/new` `/sessions` `/resume` 与
  `[session]` JSONL store（默认 `~/.deepseeknova/sessions`）管理的是**对话历史**；
  worktree 隔离的是**文件系统工作区**。两者互补：同一 worktree 内多会话仍共享
  该工作副本，跨 worktree 的会话则连工作副本都隔离。

### 删除与清理（delete / clean）

`worktree delete <name>` 先检查 worktree 内是否有未提交 / 未跟踪变更，有则
**拒绝**并提示先提交或暂存（`--force` 丢弃变更强制删除）。删除成功后新分支
（`<name>`）**保留**，输出会提示用 `git branch -D <name>` 自行清理。

`worktree clean` 清理主根 `.deepseeknova/worktrees/` 下所有由本 CLI 创建的
worktree：干净的直接删除，有未提交变更的跳过并列出原因。目录中 git 未登记的
残留（如中途失败的创建）仅提示，不自动删除。

> 全部 git 交互（`git worktree add/list/remove`、`git rev-parse`、
> `git status --porcelain` 等）经 `std::process::Command` 执行，非零退出码透传
> git stderr；在非 git 目录中运行会得到清晰报错（提示先 `git init`）。worktree
> 名须可同时作为目录名与 git 分支名（拒绝 `/`、`..`、空白字符等）。

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
enabled = true                  # 总开关：true（默认，默认安全姿态）时工具经 allow/ask/deny 门控
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

### 权限模式预设（`[permissions] mode`）

三档一键切换写工具的默认裁决强度（对齐 Codex sandbox_mode / Claude Code 权限
模式循环；`None`（缺省）保持旧行为——回退 `default_mode`，不引入静默安全回归）：

```toml
[permissions]
mode = "plan"   # plan | accept_edits | auto
                # plan：写工具（write/edit/move、shell 写形态）默认 Ask，最安全
                # accept_edits：文件编辑放行，shell 写形态仍 Ask
                # auto：写工具全部放行（显式选择信任）
```

规则优先级不变（deny > ask > allow > 预设回退）。TUI 内 `Ctrl+P` 循环切换 +
状态栏 `perm {mode}` 段 + `/mode plan|accept_edits|auto|cycle` 命令实时切换。

### 工作区信任（TrustStore）

`~/.deepseeknova/trusted.toml` 维护工作区信任清单（`TrustStore`，空存储默认
**untrusted = fail-closed**）：**untrusted 项目**的项目层 allow 规则降级为 ask
（不能静默放行陌生项目的自配置规则），`Config::load` 置位 `project_owns_rules`
识别规则来源。TUI 首次进入带项目层规则的工作区时弹信任确认浮层（y 信任落盘 /
n 不信任）；CLI 已接线（gate 与 agent 同实例 + TrustController 委托 TrustStore）。
