//! deepseeknova-config — layered configuration with TOML loading and merge semantics.
//!
//! Precedence (lowest to highest):
//!   1. Hard-coded defaults
//!   2. `~/.deepseeknova/config.toml`  (user)
//!   3. `./deepseeknova.toml`          (project)
//!   4. Environment variables       (DEEPSEEKNOVA_*)
//!   5. CLI flags                   (applied by caller)

use anyhow::Context;
use serde::{Deserialize, Serialize};

use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Top-level Config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Default model name to use when none is specified.
    #[serde(default)]
    pub default_model: Option<String>,

    /// Default max tool-call rounds (0 = use built-in default of 10).
    #[serde(default)]
    pub default_max_steps: Option<usize>,

    /// Provider backends (OpenAI-compatible, Anthropic, local, etc).
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,

    /// Named model entries with per-model parameters.
    #[serde(default)]
    pub models: Vec<ModelConfig>,

    /// Tool-specific configuration.
    #[serde(default)]
    pub tools: ToolsConfig,

    /// 代码图索引配置。
    #[serde(default)]
    pub graph: GraphConfig,

    /// 记忆引擎配置（闭环学习）。
    #[serde(default)]
    pub memory: MemoryConfig,

    /// 委派子代理配置（多智能体）。
    #[serde(default)]
    pub delegate: DelegateConfig,

    /// Agent behaviour tuning.
    #[serde(default)]
    pub agent: AgentConfig,

    /// Permission rules for tool execution.
    #[serde(default)]
    pub permissions: PermissionsConfig,

    /// Sandbox settings for shell and file tools.
    #[serde(default)]
    pub sandbox: SandboxConfig,

    /// Security policy for tool execution (capabilities, path/command/domain
    /// allow-lists, resource limits).
    #[serde(default)]
    pub security: SecurityConfig,

    /// MCP server definitions.
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,

    /// OpenTelemetry export settings (disabled by default).
    #[serde(default)]
    pub telemetry: TelemetryConfig,

    /// Role-based model pointers (main/task/compact/quick).
    #[serde(default)]
    pub model_pointers: ModelPointersConfig,

    /// Session persistence for long-task resume (B2).
    #[serde(default)]
    pub session: SessionConfig,

    /// Prompt budget guard evaluated at agent step boundaries (B2).
    #[serde(default)]
    pub budget: BudgetConfig,

    /// Pre-completion self-review gate (B3, default off).
    #[serde(default)]
    pub review: ReviewConfig,

    /// Deterministic post-write verification (default off).
    #[serde(default)]
    pub verify: VerifyConfig,

    /// 写前快照检查点（A1）。
    #[serde(default)]
    pub checkpoint: CheckpointConfig,
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Unique name for this provider (e.g. "deepseek", "openai").
    pub name: String,

    /// Provider kind: "openai", "anthropic", "ollama", "openrouter".
    pub kind: String,

    /// Base URL for the API endpoint.
    #[serde(default)]
    pub base_url: Option<String>,

    /// Default model for this provider.
    #[serde(default)]
    pub model: Option<String>,

    /// Environment variable that holds the API key.
    #[serde(default)]
    pub api_key_env: Option<String>,

    /// Optional API key directly (not recommended — prefer api_key_env).
    #[serde(default)]
    pub api_key: Option<String>,

    /// Request timeout in seconds.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,

    /// Max retries on transient failures (429, 5xx).
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,

    /// Extra headers to send with every request.
    #[serde(default)]
    pub headers: Vec<HeaderEntry>,

    /// Enable DeepSeek thinking mode.
    /// When true, sends extra_body: {"thinking": {"type": "enabled"}}.
    /// For DeepSeek providers, defaults to true in the provider factory
    /// even when this field is absent from config.
    #[serde(default)]
    pub thinking_enabled: bool,

    /// DeepSeek reasoning effort level: "low", "medium", "high", or "max".
    /// Controls the depth of the model's internal reasoning chain.
    /// Defaults to "high" for DeepSeek providers in the factory.
    #[serde(default)]
    pub reasoning_effort: Option<String>,

    /// Extra JSON body fields to include in every request to this provider.
    /// Merged into the request body at the top level.
    #[serde(default)]
    pub extra_body: Option<serde_json::Value>,
}

fn default_timeout() -> u64 {
    120
}
fn default_max_retries() -> u32 {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderEntry {
    pub name: String,
    pub value: String,
}

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Model identifier (e.g. "deepseek-v4-flash", "claude-sonnet-5-20251001").
    pub name: String,

    /// Which provider this model uses.
    pub provider: String,

    /// Context window size in tokens (informational).
    #[serde(default)]
    pub context_window: Option<u32>,

    /// Max output tokens.
    #[serde(default)]
    pub max_tokens: Option<u32>,

    /// Default temperature.
    #[serde(default)]
    pub temperature: Option<f32>,

    /// Whether this model supports streaming.
    #[serde(default = "default_true")]
    pub supports_streaming: bool,

    /// Whether this model supports tool/function calling.
    #[serde(default = "default_true")]
    pub supports_tools: bool,

    /// Whether this model supports vision (image inputs).
    #[serde(default)]
    pub supports_vision: bool,

    /// Model is only used for planning (read-only, no tool execution).
    #[serde(default)]
    pub planner_only: bool,

    /// Input (prompt) price in USD per 1M tokens. Unset = cost not estimated.
    #[serde(default)]
    pub input_price_per_mtok: Option<f64>,

    /// Output (completion, incl. reasoning) price in USD per 1M tokens.
    #[serde(default)]
    pub output_price_per_mtok: Option<f64>,

    /// Prompt-cache-hit price in USD per 1M tokens. Unset = falls back to
    /// `input_price_per_mtok` when estimating.
    #[serde(default)]
    pub cache_hit_price_per_mtok: Option<f64>,
}

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Model pointers — role-based model routing (Kode-style main/task/compact/quick)
// ---------------------------------------------------------------------------

/// Role-based model pointers. Each role optionally names an entry in
/// `[[models]]`. Unset roles fall back to `main`; an unset `main` falls back
/// to the legacy default-provider resolution, so zero-config behaviour is
/// unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelPointersConfig {
    /// Primary conversation model.
    #[serde(default)]
    pub main: Option<String>,
    /// Sub-agent / delegation model.
    #[serde(default)]
    pub task: Option<String>,
    /// History-compaction (summarize) model.
    #[serde(default)]
    pub compact: Option<String>,
    /// Fast utility model (titles, classification).
    #[serde(default)]
    pub quick: Option<String>,
}

impl ModelPointersConfig {
    fn merge(&mut self, other: ModelPointersConfig) {
        if other.main.is_some() {
            self.main = other.main;
        }
        if other.task.is_some() {
            self.task = other.task;
        }
        if other.compact.is_some() {
            self.compact = other.compact;
        }
        if other.quick.is_some() {
            self.quick = other.quick;
        }
    }

    /// Iterate (role-name, pointer) pairs for validation and routing.
    pub fn entries(&self) -> [(&'static str, &Option<String>); 4] {
        [
            ("main", &self.main),
            ("task", &self.task),
            ("compact", &self.compact),
            ("quick", &self.quick),
        ]
    }
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolsConfig {
    /// Tool-specific overrides. Key = tool name.
    #[serde(default)]
    pub overrides: Vec<ToolOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOverride {
    pub name: String,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub max_file_size: Option<u64>,
}

// ---------------------------------------------------------------------------
// Graph (code index)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphConfig {
    /// 主开关。false 时不构建索引、不注入 repo map，行为等同现状。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// repo map 的 token 预算。0 = 不注入 map（仅保留检索工具）。
    #[serde(default = "default_repo_map_tokens")]
    pub repo_map_tokens: usize,
    /// 单文件解析大小上限（字节），超过跳过。
    #[serde(default = "default_graph_max_file_size")]
    pub max_file_size: u64,
}

fn default_repo_map_tokens() -> usize {
    1024
}
fn default_graph_max_file_size() -> u64 {
    524_288
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            repo_map_tokens: 1024,
            max_file_size: 524_288,
        }
    }
}

// ---------------------------------------------------------------------------
// Memory (closed-loop learning)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// 主开关。false = 零开销，行为等同现状。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// SQLite 记忆库路径（相对工作区根）。
    #[serde(default = "default_memory_db_path")]
    pub db_path: String,
    /// 全自动沉淀开关（依赖 redact_secrets + CLI 审查入口作为前置条件）。
    #[serde(default = "default_true")]
    pub auto_learn: bool,
    /// 写入前脱敏（auto_learn 的硬前提）。
    #[serde(default = "default_true")]
    pub redact_secrets: bool,
    /// 嵌入后端：none | local | remote（P1 恒为 none）。
    #[serde(default = "default_embedder")]
    pub embedder: String,
    /// 嵌入模型名（P2 起用）。
    #[serde(default)]
    pub embed_model: String,
    /// 起点召回注入块的 token 上限。0 = 不注入，仅保留按需工具。
    #[serde(default = "default_recall_inject_tokens")]
    pub recall_inject_tokens: usize,
    /// 起点召回条数。
    #[serde(default = "default_recall_top_k")]
    pub recall_top_k: usize,
    /// 中途检索开关：新一轮开头或压缩后自动注入记忆 + 代码图命中。
    #[serde(default = "default_true")]
    pub mid_run_recall: bool,
    /// 中途检索的记忆条数上限。
    #[serde(default = "default_mid_run_recall_top_k")]
    pub mid_run_recall_top_k: usize,
    /// 中途检索的代码图实体条数上限（图索引启用时）。
    #[serde(default = "default_mid_run_graph_top_k")]
    pub mid_run_graph_top_k: usize,
    /// 中途检索注入块的 token 上限。0 = 不注入。
    #[serde(default = "default_mid_run_inject_tokens")]
    pub mid_run_inject_tokens: usize,
    /// 仅当上一轮执行过工具或本轮发生过压缩时才注入（默认 true）。
    #[serde(default = "default_true")]
    pub mid_run_require_tool_turn: bool,
    /// 触发沉淀的最小工具调用数。
    #[serde(default = "default_min_tool_calls")]
    pub min_tool_calls: usize,
    /// 触发沉淀的最小步数。
    #[serde(default = "default_min_steps")]
    pub min_steps: usize,
    /// 每日沉淀硬上限。
    #[serde(default = "default_max_distill_day")]
    pub max_distillations_per_day: u32,
    /// 每会话沉淀硬上限。
    #[serde(default = "default_max_distill_session")]
    pub max_distillations_per_session: u32,

    /// 是否在回合结束用 LLM 把任务观察蒸馏成可复用 skill/教训（默认 false，成本敏感）。
    #[serde(default)]
    pub llm_distill: bool,

    /// 蒸馏用模型名（可选；未配置回落 main provider）。
    #[serde(default)]
    pub llm_distill_model: Option<String>,

    /// 蒸馏输入的任务描述上限（字符，默认 3000）。
    #[serde(default = "default_llm_distill_max_chars")]
    pub llm_distill_max_chars: usize,
}

fn default_memory_db_path() -> String {
    ".deepseeknova/memory.db".to_string()
}
fn default_embedder() -> String {
    "none".to_string()
}
fn default_recall_inject_tokens() -> usize {
    200
}
fn default_recall_top_k() -> usize {
    3
}
fn default_mid_run_recall_top_k() -> usize {
    3
}
fn default_mid_run_graph_top_k() -> usize {
    4
}
fn default_mid_run_inject_tokens() -> usize {
    200
}
fn default_min_tool_calls() -> usize {
    5
}
fn default_min_steps() -> usize {
    3
}
fn default_max_distill_day() -> u32 {
    50
}
fn default_max_distill_session() -> u32 {
    10
}
fn default_llm_distill_max_chars() -> usize {
    3000
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            db_path: default_memory_db_path(),
            auto_learn: true,
            redact_secrets: true,
            embedder: default_embedder(),
            embed_model: String::new(),
            recall_inject_tokens: 200,
            recall_top_k: 3,
            mid_run_recall: true,
            mid_run_recall_top_k: 3,
            mid_run_graph_top_k: 4,
            mid_run_inject_tokens: 200,
            mid_run_require_tool_turn: true,
            min_tool_calls: 5,
            min_steps: 3,
            max_distillations_per_day: 50,
            max_distillations_per_session: 10,
            llm_distill: false,
            llm_distill_model: None,
            llm_distill_max_chars: default_llm_distill_max_chars(),
        }
    }
}

// ---------------------------------------------------------------------------
// Delegate (multi-agent sub-agents)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegateConfig {
    /// 主开关。false = 不注册 delegate 工具，行为等同现状。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 并发子代理上限（满员时新委派排队等待）。
    #[serde(default = "default_delegate_concurrency")]
    pub max_concurrent: usize,
    /// 子代理回传摘要的 token 上限。
    #[serde(default = "default_delegate_output_cap")]
    pub output_cap_tokens: usize,
    /// 预设覆盖/新增（按 name 匹配内置预设覆盖其字段；未匹配则新增）。
    #[serde(default)]
    pub agents: Vec<DelegateAgentOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegateAgentOverride {
    pub name: String,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    #[serde(default)]
    pub max_steps: Option<usize>,
}

fn default_delegate_concurrency() -> usize {
    2
}
fn default_delegate_output_cap() -> usize {
    2000
}

impl Default for DelegateConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_concurrent: 2,
            output_cap_tokens: 2000,
            agents: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Agent
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// System prompt override.
    #[serde(default)]
    pub system_prompt: Option<String>,

    /// Max tool-call rounds before forcing a stop.
    #[serde(default = "default_max_steps")]
    pub max_steps: usize,

    /// Token budget for conversation history before compaction triggers.
    /// 留空（None）且 `[budget] enabled=true` 时，运行时按 `budget.max_total_tokens / 2`
    /// 推导（默认 128000 → 64000）；显式设置优先；budget 关闭则不压缩。
    #[serde(default)]
    pub compaction_threshold_tokens: Option<u32>,

    /// Whether to run tools concurrently when possible.
    #[serde(default = "default_true")]
    pub concurrent_tools: bool,

    /// Whether plan mode is enabled by default.
    #[serde(default)]
    pub plan_mode_default: bool,

    /// What to do when max_steps is exhausted: "pause" (default, saves the
    /// session and emits RunEvent::Paused) or "error" (pre-B2 behavior).
    #[serde(default = "default_on_max_steps")]
    pub on_max_steps: String,

    /// Enable L3 structured LLM compaction. false = L1/L2 only (pre-B2).
    #[serde(default = "default_true")]
    pub l3_compaction: bool,

    /// Model used for L3 compaction digests. Empty = main model.
    #[serde(default)]
    pub compact_model: String,

    /// 每步按规则切换 reasoning effort（P2）：工具结果正常 → quick（thinking off），
    /// 首步/出错/回炉反馈 → high。默认关；开启需 runtime 注入 quick/high 两个 provider。
    #[serde(default)]
    pub step_effort_routing: bool,

    /// 工具结果观察压缩（P2）：超阈值的大输出由廉价模型摘要后入历史。默认关。
    #[serde(default)]
    pub observe_compress: bool,

    /// 触发观察压缩的输出大小阈值（字符）。
    #[serde(default = "default_observe_threshold")]
    pub observe_compress_threshold_chars: usize,

    /// 压缩后摘要的最大字符数。
    #[serde(default = "default_observe_max_chars")]
    pub observe_compress_max_chars: usize,

    /// 会话内只读工具结果缓存（P2）：同参读调用直接复用，写执行后失效。默认关。
    #[serde(default)]
    pub tool_cache: bool,
}

fn default_max_steps() -> usize {
    10
}
fn default_on_max_steps() -> String {
    "pause".to_string()
}
fn default_observe_threshold() -> usize {
    12_000
}
fn default_observe_max_chars() -> usize {
    4_000
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            system_prompt: None,
            max_steps: default_max_steps(),
            compaction_threshold_tokens: None,
            concurrent_tools: true,
            plan_mode_default: false,
            on_max_steps: default_on_max_steps(),
            l3_compaction: true,
            compact_model: String::new(),
            step_effort_routing: false,
            observe_compress: false,
            observe_compress_threshold_chars: default_observe_threshold(),
            observe_compress_max_chars: default_observe_max_chars(),
            tool_cache: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionsConfig {
    /// Master switch. When false (the default), the permission gate is not
    /// consulted during tool execution and tools run unconditionally (subject
    /// only to the SecurityContext capability/path checks). Set true to enforce
    /// allow/ask/deny gating.
    #[serde(default)]
    pub enabled: bool,

    /// Default mode for write tools when no rule matches.
    #[serde(default)]
    pub default_mode: PermissionMode,

    /// Optional rate limit: max gated tool calls per rolling minute.
    /// `None` disables rate limiting.
    #[serde(default)]
    pub rate_limit_per_minute: Option<u32>,

    /// Rules ordered by priority. First match wins.
    #[serde(default)]
    pub rules: Vec<PermissionRule>,
}

impl Default for PermissionsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_mode: PermissionMode::Ask,
            rate_limit_per_minute: None,
            rules: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum PermissionMode {
    #[default]
    Ask,
    Allow,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRule {
    /// Tool name to match (e.g. "bash", "read_file", "*").
    pub tool: String,

    /// Optional subject pattern (e.g. "rm *", "docs/**").
    #[serde(default)]
    pub subject: Option<String>,

    /// What to do when this rule matches.
    pub mode: PermissionMode,
}

// ---------------------------------------------------------------------------
// Sandbox
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Enable sandboxing for shell commands.
    #[serde(default)]
    pub enabled: bool,

    /// Allow network access from sandboxed commands.
    #[serde(default)]
    pub allow_network: bool,

    /// Additional directories to expose read-only inside sandbox.
    #[serde(default)]
    pub readonly_paths: Vec<String>,

    /// Additional directories to expose read-write inside sandbox.
    #[serde(default)]
    pub writable_paths: Vec<String>,

    /// Command timeout in seconds.
    #[serde(default = "default_sandbox_timeout")]
    pub timeout_secs: u64,
}

fn default_sandbox_timeout() -> u64 {
    120
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_network: false,
            readonly_paths: Vec::new(),
            writable_paths: Vec::new(),
            timeout_secs: default_sandbox_timeout(),
        }
    }
}

// ---------------------------------------------------------------------------
// Security
// ---------------------------------------------------------------------------

/// Security policy for tool execution.
///
/// Controls which capabilities tools may exercise, filesystem path
/// confinement, command/domain allow-lists, and resource limits.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// Capabilities to DISABLE (deny). Any name not in this set remains
    /// granted. Recognized names (case-insensitive):
    /// `file_read`, `file_write`, `command_execute`, `network_access`,
    /// `mcp_invoke`, `memory_read`, `memory_write`.
    #[serde(default)]
    pub disabled_capabilities: Vec<String>,

    /// Path prefixes tools are allowed to touch (in addition to the
    /// workspace root, which is always allowed).
    #[serde(default)]
    pub allowed_paths: Vec<String>,

    /// Path prefixes tools are never allowed to touch (deny takes
    /// precedence over allow).
    #[serde(default)]
    pub denied_paths: Vec<String>,

    /// Command prefixes the shell tool may execute. Empty = allow all.
    #[serde(default)]
    pub allowed_commands: Vec<String>,

    /// Domains the web_fetch tool may contact. Empty = allow all.
    #[serde(default)]
    pub allowed_domains: Vec<String>,

    /// Resource limits. Fields left as `None` fall back to library defaults.
    #[serde(default)]
    pub limits: ResourceLimitsConfig,
}

/// Optional resource-limits overrides. Any field left `None` keeps the
/// library default.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResourceLimitsConfig {
    #[serde(default)]
    pub max_files: Option<usize>,
    #[serde(default)]
    pub max_file_size: Option<u64>,
    #[serde(default)]
    pub max_total_read_bytes: Option<u64>,
    #[serde(default)]
    pub max_execution_time_secs: Option<u64>,
    #[serde(default)]
    pub max_output_bytes: Option<u64>,
    #[serde(default)]
    pub max_tool_calls: Option<usize>,
}

// ---------------------------------------------------------------------------
// MCP Server
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Logical name for this MCP server.
    pub name: String,

    /// Command to spawn (e.g. "npx", "uvx").
    pub command: String,

    /// Arguments for the command.
    #[serde(default)]
    pub args: Vec<String>,

    /// Environment variables to set.
    #[serde(default)]
    pub env: Vec<EnvEntry>,

    /// Whether this server is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvEntry {
    pub name: String,
    pub value: String,
}

// ---------------------------------------------------------------------------
// Telemetry
// ---------------------------------------------------------------------------

/// OpenTelemetry (OTLP) export configuration.
///
/// Disabled by default. When enabled, the CLI installs the
/// `deepseeknova-telemetry` subscriber instead of the plain fmt subscriber;
/// terminal log output is suppressed in that mode (the OTel registry carries
/// no fmt layer) — a known trade-off.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TelemetryConfig {
    /// Whether OTLP telemetry export is enabled (default: false).
    #[serde(default)]
    pub enabled: bool,

    /// OTLP collector endpoint (e.g. "http://localhost:4317").
    /// When unset, the exporter falls back to `http://localhost:4317`.
    #[serde(default)]
    pub otlp_endpoint: Option<String>,
}

// ---------------------------------------------------------------------------
// Session（长任务会话持久化）
// ---------------------------------------------------------------------------

/// Session persistence configuration (long-task resume).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    /// Whether chat/run sessions are persisted (default: true).
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Session store root. Empty (default) = `~/.deepseeknova/sessions`
    /// (the pre-B2 behavior); non-empty = explicit directory path.
    #[serde(default)]
    pub root: String,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            root: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Budget（step 边界上下文预算守门）
// ---------------------------------------------------------------------------

/// Prompt budget configuration, feeding `PromptBudgetController`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    /// Whether the budget guard runs at step boundaries (default: true).
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Hard context ceiling in estimated tokens (default: 128000).
    #[serde(default = "default_budget_total")]
    pub max_total_tokens: usize,

    /// Memory sub-budget in estimated tokens (default: 32000).
    #[serde(default = "default_budget_memory")]
    pub max_memory_tokens: usize,
}

fn default_budget_total() -> usize {
    128_000
}
fn default_budget_memory() -> usize {
    32_000
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_total_tokens: default_budget_total(),
            max_memory_tokens: default_budget_memory(),
        }
    }
}

// ---------------------------------------------------------------------------
// Review（完成前自审，B3）
// ---------------------------------------------------------------------------

/// Pre-completion self-review configuration (default OFF).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewConfig {
    /// Whether the pre-Done review gate runs (default: false — data-driven
    /// flip after ≥50 triggers evaluated manually).
    #[serde(default)]
    pub enabled: bool,

    /// Model for the review verdict. Empty = main provider.
    #[serde(default)]
    pub review_model: String,

    /// Cap (estimated tokens) on the diff excerpt sent to the reviewer.
    #[serde(default = "default_diff_cap")]
    pub diff_cap_tokens: usize,

    /// Fix cycles allowed before pausing for human review (default: 1).
    #[serde(default = "default_review_cycles")]
    pub max_cycles: usize,
}

fn default_diff_cap() -> usize {
    3000
}

fn default_review_cycles() -> usize {
    1
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            review_model: String::new(),
            diff_cap_tokens: default_diff_cap(),
            max_cycles: default_review_cycles(),
        }
    }
}

// ---------------------------------------------------------------------------
// Verify（完成前确定性验证，P1）
// ---------------------------------------------------------------------------

/// Deterministic verification run after file-writing turns (default OFF).
///
/// Commands run through the registered `bash` tool so sandbox, command
/// allow-lists and resource limits all apply. Failures feed back into the
/// agent loop as User messages; exceeding `max_cycles` pauses the run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyConfig {
    /// Whether the post-write verification gate runs (default: false).
    #[serde(default)]
    pub enabled: bool,

    /// Shell commands executed in order after a writing turn completes.
    #[serde(default)]
    pub commands: Vec<String>,

    /// Fix cycles allowed before pausing for human review (default: 1).
    #[serde(default = "default_verify_cycles")]
    pub max_cycles: usize,
}

fn default_verify_cycles() -> usize {
    1
}

impl Default for VerifyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            commands: Vec::new(),
            max_cycles: default_verify_cycles(),
        }
    }
}

// ---------------------------------------------------------------------------
// Checkpoint（写前快照 + 回滚，A1）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointConfig {
    /// 写类工具执行前是否快照（默认 true）。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 快照持久化路径（相对工作区根，JSONL）。
    #[serde(default = "default_checkpoint_path")]
    pub path: String,
}

fn default_checkpoint_path() -> String {
    ".deepseeknova/checkpoints.json".to_string()
}

impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: default_checkpoint_path(),
        }
    }
}

// ---------------------------------------------------------------------------
// Loading & merging
// ---------------------------------------------------------------------------

impl Config {
    /// Load from a specific file path.
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read config file: {}", path.display()))?;
        let config: Config = toml::from_str(&content)
            .with_context(|| format!("failed to parse TOML: {}", path.display()))?;
        Ok(config)
    }

    /// Load with layered precedence: defaults → user → project → env.
    pub fn load() -> anyhow::Result<Self> {
        let mut config = Config::default();

        // Layer 1: user-global config (~/.deepseeknova/config.toml)
        if let Some(user_path) = user_config_path() {
            if user_path.exists() {
                let user = Self::load_from_file(&user_path)?;
                config.merge(user);
            }
        }

        // Layer 2: project-local config (./deepseeknova.toml)
        let project_path = PathBuf::from("deepseeknova.toml");
        if project_path.exists() {
            let project = Self::load_from_file(&project_path)?;
            config.merge(project);
        }

        // Layer 3: environment variables
        config.apply_env_overrides();

        config.validate()?;

        Ok(config)
    }

    /// Merge `other` into self. Non-default values in `other` overwrite self.
    #[doc(hidden)]
    pub fn merge(&mut self, other: Config) {
        if other.default_model.is_some() {
            self.default_model = other.default_model;
        }
        if other.default_max_steps.is_some() {
            self.default_max_steps = other.default_max_steps;
        }
        if !other.providers.is_empty() {
            // Project providers replace user providers (don't merge per-entry)
            self.providers = other.providers;
        }
        if !other.models.is_empty() {
            self.models = other.models;
        }
        if !other.mcp_servers.is_empty() {
            self.mcp_servers = other.mcp_servers;
        }
        // Deep-merge sections with non-default values
        self.tools.merge(other.tools);
        self.agent.merge(other.agent);
        self.permissions.merge(other.permissions);
        self.sandbox.merge(other.sandbox);
        self.security.merge(other.security);
        self.telemetry.merge(other.telemetry);
        self.model_pointers.merge(other.model_pointers);
        self.session = other.session;
        self.budget = other.budget;
        self.review = other.review;
        self.verify = other.verify;
        self.checkpoint = other.checkpoint;
    }

    /// Apply DEEPSEEKNOVA_* environment variable overrides.
    fn apply_env_overrides(&mut self) {
        if let Ok(val) = std::env::var("DEEPSEEKNOVA_MODEL") {
            self.default_model = Some(val);
        }
        if let Ok(val) = std::env::var("DEEPSEEKNOVA_MAX_STEPS") {
            if let Ok(n) = val.parse() {
                self.default_max_steps = Some(n);
            }
        }
    }

    /// Look up a provider config by name.
    pub fn find_provider(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.name == name)
    }

    /// Look up a model config by name.
    pub fn find_model(&self, name: &str) -> Option<&ModelConfig> {
        self.models.iter().find(|m| m.name == name)
    }

    /// Resolve which provider to use for a model name.
    pub fn resolve_provider_for_model(&self, model_name: &str) -> Option<&ProviderConfig> {
        // Check model config first
        if let Some(model) = self.find_model(model_name) {
            return self.find_provider(&model.provider);
        }
        // Fall back to first provider
        self.providers.first()
    }

    /// Validate cross-references: model pointers must name a defined model,
    /// and prices must be non-negative. Called by [`Config::load`]; callers
    /// constructing configs programmatically may call it directly.
    pub fn validate(&self) -> anyhow::Result<()> {
        let names: Vec<&str> = self.models.iter().map(|m| m.name.as_str()).collect();
        for (role, ptr) in self.model_pointers.entries() {
            if let Some(model) = ptr {
                if !names.contains(&model.as_str()) {
                    anyhow::bail!(
                        "model_pointers.{role} points to unknown model '{model}' \
                         (known models: {})",
                        names.join(", ")
                    );
                }
            }
        }
        for m in &self.models {
            for (field, price) in [
                ("input_price_per_mtok", m.input_price_per_mtok),
                ("output_price_per_mtok", m.output_price_per_mtok),
                ("cache_hit_price_per_mtok", m.cache_hit_price_per_mtok),
            ] {
                if let Some(p) = price {
                    if !p.is_finite() || p < 0.0 {
                        anyhow::bail!(
                            "models.{}.{field} must be a finite value >= 0, got {p}",
                            m.name
                        );
                    }
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Per-section merge helpers
// ---------------------------------------------------------------------------

impl ToolsConfig {
    fn merge(&mut self, other: ToolsConfig) {
        if !other.overrides.is_empty() {
            self.overrides = other.overrides;
        }
    }
}

impl AgentConfig {
    fn merge(&mut self, other: AgentConfig) {
        if other.system_prompt.is_some() {
            self.system_prompt = other.system_prompt;
        }
        if other.compaction_threshold_tokens.is_some() {
            self.compaction_threshold_tokens = other.compaction_threshold_tokens;
        }
        // max_steps: project value always overrides (0 means "use default", handled at usage site)
        self.max_steps = other.max_steps;
        self.concurrent_tools = other.concurrent_tools;
        self.plan_mode_default = other.plan_mode_default;
        self.on_max_steps = other.on_max_steps;
        self.l3_compaction = other.l3_compaction;
        if !other.compact_model.is_empty() {
            self.compact_model = other.compact_model;
        }
        self.step_effort_routing = other.step_effort_routing;
        self.observe_compress = other.observe_compress;
        self.observe_compress_threshold_chars = other.observe_compress_threshold_chars;
        self.observe_compress_max_chars = other.observe_compress_max_chars;
        self.tool_cache = other.tool_cache;
    }
}

impl PermissionsConfig {
    fn merge(&mut self, other: PermissionsConfig) {
        self.enabled = other.enabled;
        self.default_mode = other.default_mode;
        if !other.rules.is_empty() {
            self.rules = other.rules;
        }
    }
}

impl TelemetryConfig {
    fn merge(&mut self, other: TelemetryConfig) {
        self.enabled = other.enabled;
        if other.otlp_endpoint.is_some() {
            self.otlp_endpoint = other.otlp_endpoint;
        }
    }
}

impl SandboxConfig {
    fn merge(&mut self, other: SandboxConfig) {
        self.enabled = other.enabled;
        self.allow_network = other.allow_network;
        if !other.readonly_paths.is_empty() {
            self.readonly_paths = other.readonly_paths;
        }
        if !other.writable_paths.is_empty() {
            self.writable_paths = other.writable_paths;
        }
        self.timeout_secs = other.timeout_secs;
    }
}

impl SecurityConfig {
    fn merge(&mut self, other: SecurityConfig) {
        if !other.disabled_capabilities.is_empty() {
            self.disabled_capabilities = other.disabled_capabilities;
        }
        if !other.allowed_paths.is_empty() {
            self.allowed_paths = other.allowed_paths;
        }
        if !other.denied_paths.is_empty() {
            self.denied_paths = other.denied_paths;
        }
        if !other.allowed_commands.is_empty() {
            self.allowed_commands = other.allowed_commands;
        }
        if !other.allowed_domains.is_empty() {
            self.allowed_domains = other.allowed_domains;
        }
        self.limits.merge(other.limits);
    }
}

impl ResourceLimitsConfig {
    fn merge(&mut self, other: ResourceLimitsConfig) {
        if other.max_files.is_some() {
            self.max_files = other.max_files;
        }
        if other.max_file_size.is_some() {
            self.max_file_size = other.max_file_size;
        }
        if other.max_total_read_bytes.is_some() {
            self.max_total_read_bytes = other.max_total_read_bytes;
        }
        if other.max_execution_time_secs.is_some() {
            self.max_execution_time_secs = other.max_execution_time_secs;
        }
        if other.max_output_bytes.is_some() {
            self.max_output_bytes = other.max_output_bytes;
        }
        if other.max_tool_calls.is_some() {
            self.max_tool_calls = other.max_tool_calls;
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn user_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".deepseeknova").join("config.toml"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_sane() {
        let cfg = Config::default();
        assert!(cfg.default_model.is_none());
        assert!(cfg.providers.is_empty());
        assert_eq!(cfg.agent.max_steps, 10);
        assert_eq!(cfg.permissions.default_mode, PermissionMode::Ask);
        assert!(!cfg.sandbox.enabled);
    }

    #[test]
    fn merge_preserves_higher_priority() {
        let mut base = Config::default();

        let override_cfg = Config {
            default_model: Some("gpt-5".into()),
            agent: AgentConfig {
                max_steps: 20,
                ..Default::default()
            },
            ..Default::default()
        };

        base.merge(override_cfg);

        assert_eq!(base.default_model.as_deref(), Some("gpt-5"));
        assert_eq!(base.agent.max_steps, 20);
    }

    #[test]
    fn find_provider_by_name() {
        let cfg = Config {
            providers: vec![ProviderConfig {
                name: "deepseek".into(),
                kind: "openai".into(),
                base_url: Some("https://api.deepseek.com".into()),
                model: Some("deepseek-v4-flash".into()),
                api_key_env: Some("DEEPSEEK_API_KEY".into()),
                api_key: None,
                timeout_secs: 120,
                max_retries: 3,
                headers: vec![],
                thinking_enabled: false,
                reasoning_effort: None,
                extra_body: None,
            }],
            ..Config::default()
        };

        assert!(cfg.find_provider("deepseek").is_some());
        assert!(cfg.find_provider("nonexistent").is_none());
    }

    #[test]
    fn security_config_merge_preserves_defaults() {
        let base = Config::default();
        // 默认：所有能力均未禁用，工作区外无额外路径/命令/域名。
        assert!(base.security.disabled_capabilities.is_empty());
        assert!(base.security.allowed_paths.is_empty());
        assert!(base.security.denied_paths.is_empty());
        assert!(base.security.allowed_commands.is_empty());
        assert!(base.security.allowed_domains.is_empty());
        assert!(base.security.limits.max_files.is_none());
    }

    #[test]
    fn security_config_merge_overrides_lists_and_limits() {
        let mut base = Config::default();
        let override_cfg = Config {
            security: crate::SecurityConfig {
                disabled_capabilities: vec!["file_write".into(), "network_access".into()],
                allowed_paths: vec!["/tmp/build".into()],
                denied_paths: vec!["/tmp/build/secret".into()],
                allowed_commands: vec!["git".into()],
                allowed_domains: vec!["api.github.com".into()],
                limits: crate::ResourceLimitsConfig {
                    max_files: Some(42),
                    max_execution_time_secs: Some(60),
                    ..Default::default()
                },
            },
            ..Default::default()
        };

        base.merge(override_cfg);

        assert_eq!(base.security.disabled_capabilities.len(), 2);
        assert!(base
            .security
            .disabled_capabilities
            .contains(&"file_write".to_string()));
        assert_eq!(base.security.allowed_paths, vec!["/tmp/build".to_string()]);
        assert_eq!(base.security.allowed_commands, vec!["git".to_string()]);
        assert_eq!(base.security.limits.max_files, Some(42));
        assert_eq!(base.security.limits.max_execution_time_secs, Some(60));
        // 未覆盖的字段保持未设置
        assert!(base.security.limits.max_file_size.is_none());
    }

    #[test]
    fn graph_config_defaults() {
        let c = Config::default();
        assert!(c.graph.enabled);
        assert_eq!(c.graph.repo_map_tokens, 1024);
        assert_eq!(c.graph.max_file_size, 524_288);
    }

    #[test]
    fn graph_config_parses_from_toml() {
        let toml = "[graph]\nenabled = false\nrepo_map_tokens = 0\n";
        let c: Config = toml::from_str(toml).unwrap();
        assert!(!c.graph.enabled);
        assert_eq!(c.graph.repo_map_tokens, 0);
        assert_eq!(c.graph.max_file_size, 524_288);
    }

    #[test]
    fn memory_config_defaults() {
        let c = Config::default();
        assert!(c.memory.enabled);
        assert!(c.memory.auto_learn);
        assert!(c.memory.redact_secrets);
        assert_eq!(c.memory.embedder, "none");
        assert_eq!(c.memory.embed_model, "");
        assert_eq!(c.memory.recall_inject_tokens, 200);
        assert_eq!(c.memory.recall_top_k, 3);
        assert_eq!(c.memory.min_tool_calls, 5);
        assert_eq!(c.memory.min_steps, 3);
        assert_eq!(c.memory.max_distillations_per_day, 50);
        assert_eq!(c.memory.max_distillations_per_session, 10);
        assert!(!c.memory.llm_distill, "LLM 蒸馏必须默认关闭（成本敏感）");
        assert_eq!(c.memory.llm_distill_model, None);
        assert_eq!(c.memory.llm_distill_max_chars, 3000);
        assert_eq!(c.memory.db_path, ".deepseeknova/memory.db");
    }

    #[test]
    fn memory_config_parses_from_toml() {
        let toml = "[memory]\nenabled = false\nauto_learn = false\nrecall_top_k = 7\nllm_distill = true\nllm_distill_model = \"deepseek-v4-flash\"\nllm_distill_max_chars = 1500\n";
        let c: Config = toml::from_str(toml).unwrap();
        assert!(!c.memory.enabled);
        assert!(!c.memory.auto_learn);
        assert_eq!(c.memory.recall_top_k, 7);
        assert!(c.memory.llm_distill);
        assert_eq!(
            c.memory.llm_distill_model.as_deref(),
            Some("deepseek-v4-flash")
        );
        assert_eq!(c.memory.llm_distill_max_chars, 1500);
        // 未覆盖字段仍取默认
        assert!(c.memory.redact_secrets);
        assert_eq!(c.memory.recall_inject_tokens, 200);
    }

    #[test]
    fn delegate_config_defaults() {
        let c = Config::default();
        assert!(c.delegate.enabled);
        assert_eq!(c.delegate.max_concurrent, 2);
        assert_eq!(c.delegate.output_cap_tokens, 2000);
        assert!(c.delegate.agents.is_empty());
    }

    #[test]
    fn delegate_config_parses_overrides() {
        let toml = "[delegate]\nenabled = false\nmax_concurrent = 3\n\n[[delegate.agents]]\nname = \"coder\"\nmax_steps = 25\n";
        let c: Config = toml::from_str(toml).unwrap();
        assert!(!c.delegate.enabled);
        assert_eq!(c.delegate.max_concurrent, 3);
        assert_eq!(c.delegate.output_cap_tokens, 2000); // 未覆盖取默认
        assert_eq!(c.delegate.agents.len(), 1);
        assert_eq!(c.delegate.agents[0].name, "coder");
        assert_eq!(c.delegate.agents[0].max_steps, Some(25));
    }

    #[test]
    fn session_budget_config_defaults() {
        let c = Config::default();
        assert!(c.session.enabled);
        assert_eq!(c.session.root, "");
        assert!(c.budget.enabled);
        assert_eq!(c.budget.max_total_tokens, 128_000);
        assert_eq!(c.budget.max_memory_tokens, 32_000);
        assert_eq!(c.agent.on_max_steps, "pause");
        assert!(c.agent.l3_compaction);
        assert_eq!(c.agent.compact_model, "");
    }

    #[test]
    fn agent_b2_fields_parse_overrides() {
        let toml = "[agent]\non_max_steps = \"error\"\nl3_compaction = false\ncompact_model = \"deepseek-chat\"\n\n[budget]\nenabled = false\nmax_total_tokens = 64000\n";
        let c: Config = toml::from_str(toml).unwrap();
        assert_eq!(c.agent.on_max_steps, "error");
        assert!(!c.agent.l3_compaction);
        assert_eq!(c.agent.compact_model, "deepseek-chat");
        assert!(!c.budget.enabled);
        assert_eq!(c.budget.max_total_tokens, 64_000);
        assert_eq!(c.budget.max_memory_tokens, 32_000);
        assert!(c.session.enabled);
    }

    #[test]
    fn agent_p2_fields_defaults_and_overrides() {
        let d = Config::default();
        assert!(!d.agent.step_effort_routing);
        assert!(!d.agent.observe_compress);
        assert_eq!(d.agent.observe_compress_threshold_chars, 12_000);
        assert_eq!(d.agent.observe_compress_max_chars, 4_000);
        assert!(!d.agent.tool_cache);

        let toml = "[agent]\nstep_effort_routing = true\nobserve_compress = true\nobserve_compress_threshold_chars = 5000\nobserve_compress_max_chars = 1000\ntool_cache = true\n";
        let c: Config = toml::from_str(toml).unwrap();
        assert!(c.agent.step_effort_routing);
        assert!(c.agent.observe_compress);
        assert_eq!(c.agent.observe_compress_threshold_chars, 5_000);
        assert_eq!(c.agent.observe_compress_max_chars, 1_000);
        assert!(c.agent.tool_cache);
    }

    #[test]
    fn memory_mid_run_fields_defaults_and_overrides() {
        let d = Config::default();
        assert!(d.memory.mid_run_recall);
        assert_eq!(d.memory.mid_run_recall_top_k, 3);
        assert_eq!(d.memory.mid_run_graph_top_k, 4);
        assert_eq!(d.memory.mid_run_inject_tokens, 200);
        assert!(d.memory.mid_run_require_tool_turn);

        let toml = "[memory]\nmid_run_recall = false\nmid_run_recall_top_k = 5\nmid_run_graph_top_k = 6\nmid_run_inject_tokens = 300\nmid_run_require_tool_turn = false\n";
        let c: Config = toml::from_str(toml).unwrap();
        assert!(!c.memory.mid_run_recall);
        assert_eq!(c.memory.mid_run_recall_top_k, 5);
        assert_eq!(c.memory.mid_run_graph_top_k, 6);
        assert_eq!(c.memory.mid_run_inject_tokens, 300);
        assert!(!c.memory.mid_run_require_tool_turn);
    }

    #[test]
    fn review_config_defaults_off() {
        let c = Config::default();
        assert!(!c.review.enabled, "review must default OFF per spec");
        assert_eq!(c.review.review_model, "");
        assert_eq!(c.review.diff_cap_tokens, 3000);
        assert_eq!(c.review.max_cycles, 1);
    }

    #[test]
    fn review_config_parses_overrides() {
        let toml =
            "[review]\nenabled = true\nreview_model = \"deepseek-chat\"\ndiff_cap_tokens = 1500\n";
        let c: Config = toml::from_str(toml).unwrap();
        assert!(c.review.enabled);
        assert_eq!(c.review.review_model, "deepseek-chat");
        assert_eq!(c.review.diff_cap_tokens, 1500);
        assert_eq!(c.review.max_cycles, 1); // 未覆盖取默认
    }

    #[test]
    fn verify_config_defaults_off() {
        let c = Config::default();
        assert!(!c.verify.enabled, "verify must default OFF per spec");
        assert!(c.verify.commands.is_empty());
        assert_eq!(c.verify.max_cycles, 1);
    }

    #[test]
    fn verify_config_parses_overrides() {
        let toml = "[verify]\nenabled = true\ncommands = [\"cargo check --quiet\", \"cargo test --quiet\"]\nmax_cycles = 2\n";
        let c: Config = toml::from_str(toml).unwrap();
        assert!(c.verify.enabled);
        assert_eq!(c.verify.commands.len(), 2);
        assert_eq!(c.verify.commands[0], "cargo check --quiet");
        assert_eq!(c.verify.max_cycles, 2);
    }

    #[test]
    fn checkpoint_config_defaults_and_overrides() {
        let d = Config::default();
        assert!(d.checkpoint.enabled);
        assert_eq!(d.checkpoint.path, ".deepseeknova/checkpoints.json");

        let toml = "[checkpoint]\nenabled = false\npath = \"custom/ck.jsonl\"\n";
        let c: Config = toml::from_str(toml).unwrap();
        assert!(!c.checkpoint.enabled);
        assert_eq!(c.checkpoint.path, "custom/ck.jsonl");
    }
}
