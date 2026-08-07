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

use std::collections::HashMap;
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

    /// 会话效能度量（SessionMetrics）落盘开关（默认 true）。
    #[serde(default)]
    pub metrics: MetricsConfig,

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

    /// Failure attribution (default off; opt-in, behavior-neutral when off).
    #[serde(default)]
    pub attribution: AttributionConfig,

    /// Deterministic post-write verification (default off).
    #[serde(default)]
    pub verify: VerifyConfig,

    /// 写前快照检查点（A1）。
    #[serde(default)]
    pub checkpoint: CheckpointConfig,

    /// 任务质量闭环（A 阶段：ToolHook 链 + 写后策略评估）。
    #[serde(default)]
    pub quality: QualityConfig,

    /// 协议增强能力包（`[protocol]` 段）：阶段门控、对抗审查、失败模式回灌/
    /// 聚类、技能 fitness 记录的统一开关。`enabled` 默认 false——关闭时
    /// Agent 行为与现状完全一致（回归防线，见协议增强设计 §3.4/§10）。
    #[serde(default)]
    pub protocol: ProtocolConfig,
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

    /// Provider 模型上下文窗口上限（tokens）。用于 TUI token 预算条等
    /// 资源可见性 UI；未配置时由 CLI 回落 `[[models]]` 同名条目。
    #[serde(default)]
    pub context_window: Option<u32>,

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

    /// Web search tool configuration (`web_search`).
    #[serde(default)]
    pub web_search: WebSearchConfig,

    /// LSP 编辑后诊断工具配置（`lsp_diagnostics`）。
    #[serde(default)]
    pub lsp: LspConfig,
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

fn default_web_search_provider() -> String {
    "ddg".to_string()
}
fn default_web_search_max_results() -> usize {
    5
}
fn default_web_search_timeout_secs() -> u64 {
    30
}

/// `web_search` 工具配置：provider 可选用官方端点或自建 SearXNG。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchConfig {
    /// 搜索后端：`ddg`（默认，无需 key）/ `tavily` / `bing` / `searxng`。
    #[serde(default = "default_web_search_provider")]
    pub provider: String,

    /// 自定义 API 地址。`searxng` 必填（如 `http://localhost:8888`）；
    /// 其它 provider 留空使用官方端点。
    #[serde(default)]
    pub base_url: Option<String>,

    /// API key 所在环境变量名（`tavily` / `bing` 需要）。
    #[serde(default)]
    pub api_key_env: Option<String>,

    /// 每次搜索返回的最大结果数。
    #[serde(default = "default_web_search_max_results")]
    pub max_results: usize,

    /// 单次搜索超时（秒）。
    #[serde(default = "default_web_search_timeout_secs")]
    pub timeout_secs: u64,
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            provider: default_web_search_provider(),
            base_url: None,
            api_key_env: None,
            max_results: default_web_search_max_results(),
            timeout_secs: default_web_search_timeout_secs(),
        }
    }
}

fn default_lsp_timeout_secs() -> u64 {
    8
}
fn default_lsp_max_file_bytes() -> usize {
    1024 * 1024
}

/// LSP 编辑后诊断工具配置（`lsp_diagnostics`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspConfig {
    /// 总开关（默认 true；关闭时工具执行返回提示而不启动语言服务器）。
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// 语言 ID → 服务器可执行文件覆盖（如 `rust = "rust-analyzer"`）。
    /// 内置映射：rust / python / go / typescript / c / cpp。
    #[serde(default)]
    pub servers: HashMap<String, String>,

    /// 等待诊断的超时（秒）。
    #[serde(default = "default_lsp_timeout_secs")]
    pub timeout_secs: u64,

    /// 单文件内容送入 LSP 的大小上限（字节），超过则跳过诊断。
    #[serde(default = "default_lsp_max_file_bytes")]
    pub max_file_bytes: usize,
}

impl Default for LspConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            servers: HashMap::new(),
            timeout_secs: default_lsp_timeout_secs(),
            max_file_bytes: default_lsp_max_file_bytes(),
        }
    }
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
    /// 嵌入模型名（`embedder=remote` 时必填，如 text-embedding-3-small）。
    #[serde(default)]
    pub embed_model: String,
    /// 嵌入服务基础 URL（`embedder=remote` 时使用；默认 OpenAI v1）。
    #[serde(default = "default_embed_base_url")]
    pub embed_base_url: String,
    /// 嵌入请求超时（秒，默认 30）。
    #[serde(default = "default_embed_timeout_secs")]
    pub embed_timeout_secs: u64,
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

    /// 自动 skill（distill draft）→ verified 的 use_count 阈值（默认 3，
    /// 对齐 skill.rs `VERIFY_USE_THRESHOLD`）。draft 仅高匹配度试用注入，
    /// `record_use` 达标后转正进入常规 recall 注入。
    #[serde(default = "default_verify_use_threshold")]
    pub verify_use_threshold: u32,

    /// verified → active 的跨会话出现次数阈值（默认 3，对齐
    /// skill.rs `ACTIVE_SESSION_THRESHOLD`）。active 长期保留、清理豁免。
    #[serde(default = "default_active_session_threshold")]
    pub active_session_threshold: u32,

    /// 自动保留的 distill draft 数量上限（默认 20，对齐 skill.rs
    /// `MAX_AUTO_DRAFT_SKILLS`）。超出部分在会话边界按 LRU 清理
    /// （仅限 distill+draft；用户手写/verified/active 豁免）。
    #[serde(default = "default_max_auto_draft_skills")]
    pub max_auto_draft_skills: usize,

    /// 非 permanent 记忆的每次衰减率（默认 0.1；`memory cleanup` 触发时应用）。
    #[serde(default = "default_decay_rate")]
    pub decay_rate: f32,

    /// archived 记忆距最后召回（无召回按创建时间）超过该天数即被
    /// `memory cleanup` 删除（默认 30）。
    #[serde(default = "default_archive_ttl_days")]
    pub archive_ttl_days: u32,

    /// 检索排序的生命周期融合权重（默认 0.3；0 = 纯 bm25，与旧行为等价）。
    #[serde(default = "default_rank_lifecycle_weight")]
    pub rank_lifecycle_weight: f64,
}

fn default_memory_db_path() -> String {
    ".deepseeknova/memory.db".to_string()
}
fn default_embedder() -> String {
    "none".to_string()
}
fn default_embed_base_url() -> String {
    "https://api.openai.com/v1".to_string()
}
fn default_embed_timeout_secs() -> u64 {
    30
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
fn default_verify_use_threshold() -> u32 {
    3
}
fn default_active_session_threshold() -> u32 {
    3
}
fn default_max_auto_draft_skills() -> usize {
    20
}
fn default_decay_rate() -> f32 {
    0.1
}
fn default_archive_ttl_days() -> u32 {
    30
}
fn default_rank_lifecycle_weight() -> f64 {
    0.3
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
            embed_base_url: default_embed_base_url(),
            embed_timeout_secs: default_embed_timeout_secs(),
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
            verify_use_threshold: default_verify_use_threshold(),
            active_session_threshold: default_active_session_threshold(),
            max_auto_draft_skills: default_max_auto_draft_skills(),
            decay_rate: default_decay_rate(),
            archive_ttl_days: default_archive_ttl_days(),
            rank_lifecycle_weight: default_rank_lifecycle_weight(),
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
    /// 参数化任务书默认值（`${{ inputs.<name> }}` 占位符），调用方传值优先。
    /// 仅对已声明 inputs 的预设生效，simple 预设的多余键被忽略。
    #[serde(default)]
    pub inputs: Option<Vec<InputOverride>>,
}

/// 单个参数化输入覆盖（名称 + 默认值）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputOverride {
    pub name: String,
    pub value: String,
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

    /// Auto 模型+思考路由：每轮（新用户消息）先由廉价模型决定
    /// flash/pro 与 thinking off/high/max，再执行真实调用。默认关。
    #[serde(default)]
    pub auto_route: bool,

    /// 路由决策用的模型名；None 时用 quick 指针（未配置回退 main 指针）。
    #[serde(default)]
    pub auto_router_model: Option<String>,

    /// 路由调用送入的上下文最大字符数（从最新用户消息截取）。
    #[serde(default = "default_auto_router_max_chars")]
    pub auto_router_max_chars: usize,

    /// 触发观察压缩的输出大小阈值（字符）。
    #[serde(default = "default_observe_threshold")]
    pub observe_compress_threshold_chars: usize,

    /// 压缩后摘要的最大字符数。
    #[serde(default = "default_observe_max_chars")]
    pub observe_compress_max_chars: usize,

    /// 会话内只读工具结果缓存（P2）：同参读调用直接复用，写执行后失效。默认关。
    #[serde(default)]
    pub tool_cache: bool,

    /// 失败回炉前显式 LLM 反思（P1 验证 / B3 审查失败时触发）。默认 true：
    /// 只失败循环触发，失败路径本就昂贵；可关。
    #[serde(default = "default_true")]
    pub reflect_on_failure: bool,

    /// 反思用模型名（可选；未配置回落 main provider）。
    #[serde(default)]
    pub reflect_model: Option<String>,

    /// 反思输入的最后完成文本上限（字符，默认 4000）。
    #[serde(default = "default_reflect_max_chars")]
    pub reflect_max_chars: usize,
}

fn default_max_steps() -> usize {
    25
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
fn default_auto_router_max_chars() -> usize {
    6_000
}
fn default_reflect_max_chars() -> usize {
    4000
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
            auto_route: false,
            auto_router_model: None,
            auto_router_max_chars: default_auto_router_max_chars(),
            observe_compress: false,
            observe_compress_threshold_chars: default_observe_threshold(),
            observe_compress_max_chars: default_observe_max_chars(),
            tool_cache: false,
            reflect_on_failure: true,
            reflect_model: None,
            reflect_max_chars: default_reflect_max_chars(),
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
// Metrics（会话效能度量）
// ---------------------------------------------------------------------------

/// SessionMetrics 配置：run 结束时把执行面 + 成本面报告写入
/// `.deepseeknova/metrics/`（默认开启，用户可关）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsConfig {
    /// 是否在 run 结束时生成会话报告并落盘（默认 true）。
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// `.deepseeknova/metrics/` 目录保留的报告数上限（默认 100）。落盘后若
    /// 超出上限，删除最旧的报告文件（按文件修改时间，旧者先删），防止
    /// chat 每轮落盘导致的长期累积。
    #[serde(default = "default_max_reports")]
    pub max_reports: usize,
}

fn default_max_reports() -> usize {
    100
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_reports: default_max_reports(),
        }
    }
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
// Attribution（失败归因重试）
// ---------------------------------------------------------------------------

/// 失败归因配置（默认 OFF——新行为不改变既有运行语义，显式开启后生效）：
/// 子代理失败 / verify/review 达上限 Paused 前，先由 LLM 归因
/// （Retry/Degrade/Abort），再决定重试方式。归因受硬预算约束防烧 token。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributionConfig {
    /// 是否启用失败归因（默认 false，行为零变化）。
    #[serde(default)]
    pub enabled: bool,

    /// Retry/Degrade 路径的重试次数上限（默认 1，共 2 次尝试）。
    #[serde(default = "default_attribution_retries")]
    pub max_retries: usize,

    /// 单次 run 内归因调用次数上限（默认 3，防烧 token）。
    #[serde(default = "default_attribution_calls")]
    pub max_attributions: usize,

    /// Degrade 降级映射（agent 名 → 目标预设名）；未映射时按 Retry 处理。
    #[serde(default)]
    pub degrade_map: std::collections::HashMap<String, String>,
}

fn default_attribution_retries() -> usize {
    1
}

fn default_attribution_calls() -> usize {
    3
}

impl Default for AttributionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_retries: default_attribution_retries(),
            max_attributions: default_attribution_calls(),
            degrade_map: std::collections::HashMap::new(),
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

    /// 确定性命令通过后是否再用 LLM 验证最终产出（默认 false，成本敏感）。
    #[serde(default)]
    pub llm: bool,

    /// LLM 验证用模型名（可选；未配置回落 main provider）。
    #[serde(default)]
    pub llm_model: Option<String>,

    /// LLM 验证的完成文本输入上限（字符，默认 4000）。
    #[serde(default = "default_verify_llm_max_chars")]
    pub llm_max_chars: usize,
}

fn default_verify_cycles() -> usize {
    1
}

fn default_verify_llm_max_chars() -> usize {
    4000
}

impl Default for VerifyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            commands: Vec::new(),
            max_cycles: default_verify_cycles(),
            llm: false,
            llm_model: None,
            llm_max_chars: default_verify_llm_max_chars(),
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
// Quality（任务质量闭环 A 阶段：ToolHook 链 + 写后策略评估）
// ---------------------------------------------------------------------------

/// 任务质量闭环配置。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct QualityConfig {
    /// 质量钩子总开关（默认 true；关闭时 agent 行为零变化）。
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for QualityConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

// ---------------------------------------------------------------------------
// Protocol
// ---------------------------------------------------------------------------

/// 协议增强能力包配置（`[protocol]` 段，协议增强设计 §3.4）。
///
/// `enabled` 为总开关：门控（runtime `attach_protocol_gates` 解析 `gates`
/// 力度表并装配 `Agent::with_protocol_gates`）、对抗审查（`adversarial_review`
/// → `Agent::with_adversarial_review`）、失败模式回灌/聚类与技能 fitness 记录
/// （runtime 装配，见协议增强设计 §5/§6）均挂此键。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProtocolConfig {
    /// 协议能力包总开关（默认 false；关闭时 agent 行为与现状完全一致）。
    #[serde(default)]
    pub enabled: bool,

    /// 门控力度表：`gates.<name> = "hard" | "soft" | "off"`（如
    /// `plan-before-execute` / `verify-evidence` / `distill-on-complex` /
    /// `drift-detection`）；缺省条目用内置默认表（见 agent 阶段3）。
    /// 字符串而非枚举，便于向前兼容新增力度与未知门名（未知门 warn 忽略）。
    #[serde(default)]
    pub gates: HashMap<String, String>,

    /// 会话结束对抗审查子代理委派开关（默认 false；触发条件见设计 §4.2）。
    #[serde(default)]
    pub adversarial_review: bool,
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
        self.memory.merge(other.memory);
        self.delegate.merge(other.delegate);
        self.session = other.session;
        self.budget = other.budget;
        self.review = other.review;
        self.verify = other.verify;
        self.checkpoint = other.checkpoint;
        self.metrics.merge(other.metrics);
        self.attribution.merge(other.attribution);
        self.quality.merge(other.quality);
        self.protocol.merge(other.protocol);
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
        self.web_search.merge(other.web_search);
        self.lsp.merge(other.lsp);
    }
}

impl WebSearchConfig {
    fn merge(&mut self, other: WebSearchConfig) {
        let d = WebSearchConfig::default();
        if other.provider != d.provider {
            self.provider = other.provider;
        }
        if other.base_url.is_some() {
            self.base_url = other.base_url;
        }
        if other.api_key_env.is_some() {
            self.api_key_env = other.api_key_env;
        }
        if other.max_results != d.max_results {
            self.max_results = other.max_results;
        }
        if other.timeout_secs != d.timeout_secs {
            self.timeout_secs = other.timeout_secs;
        }
    }
}

impl LspConfig {
    fn merge(&mut self, other: LspConfig) {
        let d = LspConfig::default();
        if other.enabled != d.enabled {
            self.enabled = other.enabled;
        }
        if !other.servers.is_empty() {
            self.servers.extend(other.servers);
        }
        if other.timeout_secs != d.timeout_secs {
            self.timeout_secs = other.timeout_secs;
        }
        if other.max_file_bytes != d.max_file_bytes {
            self.max_file_bytes = other.max_file_bytes;
        }
    }
}

impl MemoryConfig {
    /// 深度合并 `[memory]`：仅当 `other` 字段非默认值时才覆盖，避免项目层
    /// 的缺省值清掉用户层显式配置。bool 开关按「显式 true/false 均覆盖」
    /// 处理（默认值即 false 的开关，项目层显式 false 与默认无法区分，保持
    /// 与 agent 同款 wholesale 语义）。
    fn merge(&mut self, other: MemoryConfig) {
        let d = MemoryConfig::default();
        if other.enabled != d.enabled {
            self.enabled = other.enabled;
        }
        if other.db_path != d.db_path {
            self.db_path = other.db_path;
        }
        if other.auto_learn != d.auto_learn {
            self.auto_learn = other.auto_learn;
        }
        if other.redact_secrets != d.redact_secrets {
            self.redact_secrets = other.redact_secrets;
        }
        if other.embedder != d.embedder {
            self.embedder = other.embedder;
        }
        if !other.embed_model.is_empty() {
            self.embed_model = other.embed_model;
        }
        if other.embed_base_url != d.embed_base_url {
            self.embed_base_url = other.embed_base_url;
        }
        if other.embed_timeout_secs != d.embed_timeout_secs {
            self.embed_timeout_secs = other.embed_timeout_secs;
        }
        if other.recall_inject_tokens != d.recall_inject_tokens {
            self.recall_inject_tokens = other.recall_inject_tokens;
        }
        if other.recall_top_k != d.recall_top_k {
            self.recall_top_k = other.recall_top_k;
        }
        if other.mid_run_recall != d.mid_run_recall {
            self.mid_run_recall = other.mid_run_recall;
        }
        if other.mid_run_recall_top_k != d.mid_run_recall_top_k {
            self.mid_run_recall_top_k = other.mid_run_recall_top_k;
        }
        if other.mid_run_graph_top_k != d.mid_run_graph_top_k {
            self.mid_run_graph_top_k = other.mid_run_graph_top_k;
        }
        if other.mid_run_inject_tokens != d.mid_run_inject_tokens {
            self.mid_run_inject_tokens = other.mid_run_inject_tokens;
        }
        if other.mid_run_require_tool_turn != d.mid_run_require_tool_turn {
            self.mid_run_require_tool_turn = other.mid_run_require_tool_turn;
        }
        if other.min_tool_calls != d.min_tool_calls {
            self.min_tool_calls = other.min_tool_calls;
        }
        if other.min_steps != d.min_steps {
            self.min_steps = other.min_steps;
        }
        if other.max_distillations_per_day != d.max_distillations_per_day {
            self.max_distillations_per_day = other.max_distillations_per_day;
        }
        if other.max_distillations_per_session != d.max_distillations_per_session {
            self.max_distillations_per_session = other.max_distillations_per_session;
        }
        if other.llm_distill != d.llm_distill {
            self.llm_distill = other.llm_distill;
        }
        if other.llm_distill_model.is_some() {
            self.llm_distill_model = other.llm_distill_model;
        }
        if other.llm_distill_max_chars != d.llm_distill_max_chars {
            self.llm_distill_max_chars = other.llm_distill_max_chars;
        }
        if other.verify_use_threshold != d.verify_use_threshold {
            self.verify_use_threshold = other.verify_use_threshold;
        }
        if other.active_session_threshold != d.active_session_threshold {
            self.active_session_threshold = other.active_session_threshold;
        }
        if other.max_auto_draft_skills != d.max_auto_draft_skills {
            self.max_auto_draft_skills = other.max_auto_draft_skills;
        }
        if other.decay_rate != d.decay_rate {
            self.decay_rate = other.decay_rate;
        }
        if other.archive_ttl_days != d.archive_ttl_days {
            self.archive_ttl_days = other.archive_ttl_days;
        }
        if other.rank_lifecycle_weight != d.rank_lifecycle_weight {
            self.rank_lifecycle_weight = other.rank_lifecycle_weight;
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
        self.auto_route = other.auto_route;
        if other.auto_router_model.is_some() {
            self.auto_router_model = other.auto_router_model;
        }
        self.auto_router_max_chars = other.auto_router_max_chars;
        self.observe_compress = other.observe_compress;
        self.observe_compress_threshold_chars = other.observe_compress_threshold_chars;
        self.observe_compress_max_chars = other.observe_compress_max_chars;
        self.tool_cache = other.tool_cache;
        self.reflect_on_failure = other.reflect_on_failure;
        if other.reflect_model.is_some() {
            self.reflect_model = other.reflect_model;
        }
        self.reflect_max_chars = other.reflect_max_chars;
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

impl MetricsConfig {
    /// 深度合并 `[metrics]`：仅当 `other` 字段非默认值时才覆盖，避免项目层
    /// 的缺省值清掉用户层显式配置（与 MemoryConfig 同款模式；`max_reports`
    /// 必须走同一路径，否则项目层只写 `enabled=false` 会重置用户层上限）。
    fn merge(&mut self, other: MetricsConfig) {
        let d = MetricsConfig::default();
        if other.enabled != d.enabled {
            self.enabled = other.enabled;
        }
        if other.max_reports != d.max_reports {
            self.max_reports = other.max_reports;
        }
    }
}

impl DelegateConfig {
    /// 深度合并 `[delegate]`：字段非默认值才覆盖；`agents` 仅当项目层显式
    /// 提供覆盖/新增时整体替换（与 providers/models 的“显式列表替换”语义
    /// 一致），避免项目层缺省空列表清掉用户层预设。
    fn merge(&mut self, other: DelegateConfig) {
        let d = DelegateConfig::default();
        if other.enabled != d.enabled {
            self.enabled = other.enabled;
        }
        if other.max_concurrent != d.max_concurrent {
            self.max_concurrent = other.max_concurrent;
        }
        if other.output_cap_tokens != d.output_cap_tokens {
            self.output_cap_tokens = other.output_cap_tokens;
        }
        if !other.agents.is_empty() {
            self.agents = other.agents;
        }
    }
}

impl AttributionConfig {
    /// 深度合并 `[attribution]`：与 MetricsConfig 同款非默认值覆盖模式。
    /// 不能用整体赋值——项目层存在但未写 `[attribution]` 时，缺省
    /// `enabled=false` 会把用户层显式开启重置掉。
    fn merge(&mut self, other: AttributionConfig) {
        let d = AttributionConfig::default();
        if other.enabled != d.enabled {
            self.enabled = other.enabled;
        }
        if other.max_retries != d.max_retries {
            self.max_retries = other.max_retries;
        }
        if other.max_attributions != d.max_attributions {
            self.max_attributions = other.max_attributions;
        }
        if !other.degrade_map.is_empty() {
            self.degrade_map.extend(other.degrade_map);
        }
    }
}

impl QualityConfig {
    /// 深度合并 `[quality]`：非默认值才覆盖（默认 true），项目层缺省不能
    /// 重置用户层显式关闭。
    fn merge(&mut self, other: QualityConfig) {
        let d = QualityConfig::default();
        if other.enabled != d.enabled {
            self.enabled = other.enabled;
        }
    }
}

impl ProtocolConfig {
    /// 深度合并 `[protocol]`：开关非默认值才覆盖；gates 按门名逐项叠加
    /// （项目层覆盖同名门，用户层未提及的门保留）。
    fn merge(&mut self, other: ProtocolConfig) {
        let d = ProtocolConfig::default();
        if other.enabled != d.enabled {
            self.enabled = other.enabled;
        }
        if !other.gates.is_empty() {
            self.gates.extend(other.gates);
        }
        if other.adversarial_review != d.adversarial_review {
            self.adversarial_review = other.adversarial_review;
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
        assert_eq!(cfg.agent.max_steps, 25);
        assert_eq!(cfg.permissions.default_mode, PermissionMode::Ask);
        assert!(!cfg.sandbox.enabled);
        assert!(cfg.metrics.enabled);
        // 协议增强：默认关闭（行为零变化回归防线）。
        assert!(!cfg.protocol.enabled);
        assert!(cfg.protocol.gates.is_empty());
        assert!(!cfg.protocol.adversarial_review);
    }

    #[test]
    fn protocol_config_defaults_when_absent() {
        // 旧配置无 [protocol] 段 → serde default 全缺省，兼容不破坏。
        let cfg: Config = toml::from_str(
            r#"
            [metrics]
            enabled = false
            "#,
        )
        .unwrap();
        assert!(!cfg.protocol.enabled);
        assert!(cfg.protocol.gates.is_empty());
        assert!(!cfg.protocol.adversarial_review);
        assert!(!cfg.metrics.enabled, "unrelated section still parses");
    }

    #[test]
    fn protocol_config_explicit_parse() {
        let cfg: Config = toml::from_str(
            r#"
            [protocol]
            enabled = true
            adversarial_review = true

            [protocol.gates]
            verify-evidence = "hard"
            plan-before-execute = "soft"
            drift-detection = "off"
            "#,
        )
        .unwrap();
        assert!(cfg.protocol.enabled);
        assert!(cfg.protocol.adversarial_review);
        assert_eq!(cfg.protocol.gates.get("verify-evidence").unwrap(), "hard");
        assert_eq!(
            cfg.protocol.gates.get("plan-before-execute").unwrap(),
            "soft"
        );
        assert_eq!(cfg.protocol.gates.get("drift-detection").unwrap(), "off");
        assert_eq!(cfg.protocol.gates.len(), 3);
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
                context_window: None,
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
        assert_eq!(c.memory.embed_base_url, "https://api.openai.com/v1");
        assert_eq!(c.memory.embed_timeout_secs, 30);
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
        let toml = "[memory]\nenabled = false\nauto_learn = false\nrecall_top_k = 7\nembed_base_url = \"http://localhost:1234/v1\"\nembed_timeout_secs = 5\nllm_distill = true\nllm_distill_model = \"deepseek-v4-flash\"\nllm_distill_max_chars = 1500\n";
        let c: Config = toml::from_str(toml).unwrap();
        assert!(!c.memory.enabled);
        assert!(!c.memory.auto_learn);
        assert_eq!(c.memory.recall_top_k, 7);
        assert_eq!(c.memory.embed_base_url, "http://localhost:1234/v1");
        assert_eq!(c.memory.embed_timeout_secs, 5);
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
    fn embed_fields_merge_preserves_user_values_when_unset() {
        let mut base = Config::default();
        base.memory.embed_base_url = "http://user/v1".to_string();
        base.memory.embed_timeout_secs = 9;

        // 项目层只显式设置 timeout → 不覆盖用户层的 base_url。
        let project = Config {
            memory: MemoryConfig {
                embed_timeout_secs: 5,
                ..Default::default()
            },
            ..Default::default()
        };
        base.merge(project);
        assert_eq!(
            base.memory.embed_base_url, "http://user/v1",
            "未设置字段必须保留"
        );
        assert_eq!(base.memory.embed_timeout_secs, 5, "显式字段必须覆盖");
    }

    #[test]
    fn embed_fields_merge_overrides_when_explicit() {
        let mut base = Config::default();
        let project = Config {
            memory: MemoryConfig {
                embed_base_url: "http://project/v1".to_string(),
                embed_timeout_secs: 7,
                ..Default::default()
            },
            ..Default::default()
        };
        base.merge(project);
        assert_eq!(base.memory.embed_base_url, "http://project/v1");
        assert_eq!(base.memory.embed_timeout_secs, 7);
        // 未设置字段仍是默认（不误写）。
        assert_eq!(base.memory.embed_model, "");
    }

    #[test]
    fn memory_skill_thresholds_default_and_parse() {
        let d = Config::default();
        assert_eq!(d.memory.verify_use_threshold, 3);
        assert_eq!(d.memory.active_session_threshold, 3);
        assert_eq!(d.memory.max_auto_draft_skills, 20);

        let toml = "[memory]\nverify_use_threshold = 5\nactive_session_threshold = 7\nmax_auto_draft_skills = 12\n";
        let c: Config = toml::from_str(toml).unwrap();
        assert_eq!(c.memory.verify_use_threshold, 5);
        assert_eq!(c.memory.active_session_threshold, 7);
        assert_eq!(c.memory.max_auto_draft_skills, 12);
        // 未覆盖字段仍取默认
        assert_eq!(c.memory.min_tool_calls, 5);
        assert_eq!(c.memory.recall_top_k, 3);
    }

    #[test]
    fn memory_merge_preserves_user_layer_for_unset_fields() {
        let mut base = Config::default();
        base.memory.verify_use_threshold = 9;
        base.memory.recall_top_k = 7;

        // 项目层只显式设置 min_steps → 不覆盖用户层的阈值与 recall_top_k
        let project = Config {
            memory: MemoryConfig {
                min_steps: 8,
                ..Default::default()
            },
            ..Default::default()
        };
        base.merge(project);
        assert_eq!(
            base.memory.verify_use_threshold, 9,
            "未设置字段必须保留用户层值"
        );
        assert_eq!(base.memory.recall_top_k, 7);
        assert_eq!(base.memory.min_steps, 8);

        // 项目层显式设置阈值 → 覆盖
        let project2 = Config {
            memory: MemoryConfig {
                verify_use_threshold: 11,
                ..Default::default()
            },
            ..Default::default()
        };
        base.merge(project2);
        assert_eq!(base.memory.verify_use_threshold, 11);
    }

    #[test]
    fn memory_lifecycle_fields_defaults_and_merge() {
        let d = Config::default();
        assert_eq!(d.memory.decay_rate, 0.1);
        assert_eq!(d.memory.archive_ttl_days, 30);
        assert_eq!(d.memory.rank_lifecycle_weight, 0.3);

        let toml =
            "[memory]\ndecay_rate = 0.2\narchive_ttl_days = 7\nrank_lifecycle_weight = 0.0\n";
        let c: Config = toml::from_str(toml).unwrap();
        assert_eq!(c.memory.decay_rate, 0.2);
        assert_eq!(c.memory.archive_ttl_days, 7);
        assert_eq!(c.memory.rank_lifecycle_weight, 0.0);

        // merge：项目层显式设置覆盖；未设置保留用户层值。
        let mut base = Config::default();
        base.memory.decay_rate = 0.05;
        base.memory.archive_ttl_days = 60;
        base.merge(Config {
            memory: MemoryConfig {
                rank_lifecycle_weight: 0.0,
                ..Default::default()
            },
            ..Default::default()
        });
        assert_eq!(base.memory.decay_rate, 0.05, "未设置字段必须保留用户层值");
        assert_eq!(base.memory.archive_ttl_days, 60);
        assert_eq!(base.memory.rank_lifecycle_weight, 0.0, "显式设置必须覆盖");
    }

    #[test]
    fn attribution_config_defaults_off_and_parses() {
        let d = Config::default();
        assert!(
            !d.attribution.enabled,
            "attribution 必须默认关闭（行为零变化）"
        );
        assert_eq!(d.attribution.max_retries, 1);
        assert_eq!(d.attribution.max_attributions, 3);
        assert!(d.attribution.degrade_map.is_empty());

        let toml = "[attribution]\nenabled = true\nmax_retries = 2\nmax_attributions = 5\n\n[attribution.degrade_map]\nresearcher = \"explorer\"\n";
        let c: Config = toml::from_str(toml).unwrap();
        assert!(c.attribution.enabled);
        assert_eq!(c.attribution.max_retries, 2);
        assert_eq!(c.attribution.max_attributions, 5);
        assert_eq!(
            c.attribution
                .degrade_map
                .get("researcher")
                .map(String::as_str),
            Some("explorer")
        );
    }

    #[test]
    fn attribution_merge_propagates_through_layers() {
        let mut base = Config::default();
        let user = Config {
            attribution: AttributionConfig {
                enabled: true,
                max_attributions: 7,
                ..Default::default()
            },
            ..Default::default()
        };
        base.merge(user);
        assert!(base.attribution.enabled);
        assert_eq!(base.attribution.max_attributions, 7);
        assert_eq!(base.attribution.max_retries, 1, "未覆盖字段保持默认");
    }

    #[test]
    fn attribution_merge_keeps_user_enable_when_project_lacks_section() {
        // 项目层存在但未写 [attribution]：整体赋值会把用户层 enabled=true
        // 重置为默认 false；字段级非默认覆盖必须保留用户显式开启。
        let mut base = Config::default();
        base.merge(Config {
            attribution: AttributionConfig {
                enabled: true,
                max_attributions: 7,
                ..Default::default()
            },
            ..Default::default()
        });
        let project = Config {
            memory: MemoryConfig {
                min_steps: 8,
                ..Default::default()
            },
            ..Default::default()
        };
        base.merge(project);
        assert!(base.attribution.enabled, "项目层缺省不得清掉用户显式开启");
        assert_eq!(base.attribution.max_attributions, 7);
        assert_eq!(base.attribution.max_retries, 1);
    }

    #[test]
    fn protocol_and_quality_merge_through_layers() {
        // 用户层开启协议 + 关闭质量；项目层只改门力度，不得重置开关。
        let mut base = Config::default();
        let user = Config {
            protocol: ProtocolConfig {
                enabled: true,
                gates: HashMap::from([
                    ("verify-evidence".to_string(), "soft".to_string()),
                    ("drift-detection".to_string(), "off".to_string()),
                ]),
                adversarial_review: true,
            },
            quality: QualityConfig { enabled: false },
            ..Default::default()
        };
        base.merge(user);
        assert!(base.protocol.enabled);
        assert!(base.protocol.adversarial_review);
        assert!(!base.quality.enabled);

        // 项目层显式改 verify-evidence 为 hard：同名门覆盖，其余门保留。
        let project = Config {
            protocol: ProtocolConfig {
                enabled: false, // 未显式开启（默认值）→ 不得覆盖用户层 true
                gates: HashMap::from([("verify-evidence".to_string(), "hard".to_string())]),
                adversarial_review: false, // 默认值 → 不得覆盖用户层 true
            },
            ..Default::default()
        };
        base.merge(project);
        assert!(
            base.protocol.enabled,
            "项目层缺省 enabled=false 不得覆盖用户层 true"
        );
        assert!(base.protocol.adversarial_review);
        assert_eq!(
            base.protocol
                .gates
                .get("verify-evidence")
                .map(String::as_str),
            Some("hard")
        );
        assert_eq!(
            base.protocol
                .gates
                .get("drift-detection")
                .map(String::as_str),
            Some("off")
        );

        // 项目层显式 enabled=true / 用户层未设 → 覆盖为 true。
        let mut base2 = Config::default();
        let project2 = Config {
            protocol: ProtocolConfig {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        };
        base2.merge(project2);
        assert!(base2.protocol.enabled);
    }

    #[test]
    fn delegate_merge_keeps_user_agents_when_project_lacks_agents() {
        let mut base = Config::default();
        let user = Config {
            delegate: DelegateConfig {
                enabled: true,
                max_concurrent: 5,
                output_cap_tokens: 2000,
                agents: vec![DelegateAgentOverride {
                    name: "coder".into(),
                    system_prompt: None,
                    tools: None,
                    max_steps: None,
                    inputs: Some(vec![InputOverride {
                        name: "path".into(),
                        value: "src/lib.rs".into(),
                    }]),
                }],
            },
            ..Default::default()
        };
        base.merge(user);
        assert_eq!(base.delegate.max_concurrent, 5);
        assert_eq!(base.delegate.agents.len(), 1);

        // 项目层缺省 [delegate] → 用户层预设保留（enabled/max_concurrent 默认不覆盖）。
        let project = Config {
            memory: MemoryConfig {
                min_steps: 8,
                ..Default::default()
            },
            ..Default::default()
        };
        base.merge(project);
        assert!(base.delegate.enabled);
        assert_eq!(base.delegate.max_concurrent, 5);
        assert_eq!(base.delegate.agents.len(), 1);

        // 项目层显式提供 agents → 整体替换。
        let project2 = Config {
            delegate: DelegateConfig {
                enabled: false,
                max_concurrent: 3,
                output_cap_tokens: 2000,
                agents: vec![DelegateAgentOverride {
                    name: "reviewer".into(),
                    system_prompt: None,
                    tools: None,
                    max_steps: None,
                    inputs: None,
                }],
            },
            ..Default::default()
        };
        base.merge(project2);
        assert!(!base.delegate.enabled);
        assert_eq!(base.delegate.max_concurrent, 3);
        assert_eq!(base.delegate.agents.len(), 1);
        assert_eq!(base.delegate.agents[0].name, "reviewer");
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
        let toml = "[delegate]\nenabled = false\nmax_concurrent = 3\n\n[[delegate.agents]]\nname = \"coder\"\nmax_steps = 25\n\n[[delegate.agents.inputs]]\nname = \"path\"\nvalue = \"src/lib.rs\"\n";
        let c: Config = toml::from_str(toml).unwrap();
        assert!(!c.delegate.enabled);
        assert_eq!(c.delegate.max_concurrent, 3);
        assert_eq!(c.delegate.output_cap_tokens, 2000); // 未覆盖取默认
        assert_eq!(c.delegate.agents.len(), 1);
        assert_eq!(c.delegate.agents[0].name, "coder");
        assert_eq!(c.delegate.agents[0].max_steps, Some(25));
        let inputs = c.delegate.agents[0].inputs.as_ref().unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].name, "path");
        assert_eq!(inputs[0].value, "src/lib.rs");
    }

    #[test]
    fn metrics_config_parses_and_defaults() {
        let c = Config::default();
        assert!(c.metrics.enabled);
        assert_eq!(c.metrics.max_reports, 100, "留存上限默认 100");
        let c: Config = toml::from_str("[metrics]\nenabled = false\n").unwrap();
        assert!(!c.metrics.enabled);
        assert_eq!(c.metrics.max_reports, 100, "未覆盖字段取默认");
        let c: Config = toml::from_str("[metrics]\nmax_reports = 7\n").unwrap();
        assert_eq!(c.metrics.max_reports, 7);
        assert!(c.metrics.enabled, "enabled 未设置仍为默认 true");
    }

    #[test]
    fn metrics_merge_preserves_user_layer_for_unset_fields() {
        let mut base = Config::default();
        base.metrics.max_reports = 50;

        // 项目层只显式设置 enabled=false → 不覆盖用户层 max_reports
        let project = Config {
            metrics: MetricsConfig {
                enabled: false,
                ..Default::default()
            },
            ..Default::default()
        };
        base.merge(project);
        assert!(!base.metrics.enabled, "enabled 显式 false 必须覆盖");
        assert_eq!(base.metrics.max_reports, 50, "未设置字段必须保留用户层值");

        // 项目层显式设置 max_reports → 覆盖
        let project2 = Config {
            metrics: MetricsConfig {
                enabled: false,
                max_reports: 10,
            },
            ..Default::default()
        };
        base.merge(project2);
        assert_eq!(base.metrics.max_reports, 10);
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
        assert!(c.agent.reflect_on_failure, "反思闭环默认开");
        assert_eq!(c.agent.reflect_model, None);
        assert_eq!(c.agent.reflect_max_chars, 4000);
    }

    #[test]
    fn agent_reflect_fields_parse_overrides() {
        let toml = "[agent]\nreflect_on_failure = false\nreflect_model = \"deepseek-v4-flash\"\nreflect_max_chars = 2000\n";
        let c: Config = toml::from_str(toml).unwrap();
        assert!(!c.agent.reflect_on_failure);
        assert_eq!(c.agent.reflect_model.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(c.agent.reflect_max_chars, 2000);
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
        assert!(!d.agent.auto_route);
        assert!(d.agent.auto_router_model.is_none());
        assert_eq!(d.agent.auto_router_max_chars, 6_000);

        let toml = "[agent]\nstep_effort_routing = true\nobserve_compress = true\nobserve_compress_threshold_chars = 5000\nobserve_compress_max_chars = 1000\ntool_cache = true\nauto_route = true\nauto_router_model = \"deepseek-v4-flash\"\nauto_router_max_chars = 4000\n";
        let c: Config = toml::from_str(toml).unwrap();
        assert!(c.agent.step_effort_routing);
        assert!(c.agent.observe_compress);
        assert_eq!(c.agent.observe_compress_threshold_chars, 5_000);
        assert_eq!(c.agent.observe_compress_max_chars, 1_000);
        assert!(c.agent.tool_cache);
        assert!(c.agent.auto_route);
        assert_eq!(
            c.agent.auto_router_model.as_deref(),
            Some("deepseek-v4-flash")
        );
        assert_eq!(c.agent.auto_router_max_chars, 4_000);
    }

    #[test]
    fn tools_web_search_and_lsp_defaults_and_parse() {
        let d = Config::default();
        assert_eq!(d.tools.web_search.provider, "ddg");
        assert_eq!(d.tools.web_search.max_results, 5);
        assert!(d.tools.lsp.enabled);
        assert_eq!(d.tools.lsp.timeout_secs, 8);
        assert!(d.tools.lsp.servers.is_empty());

        let toml = r#"
            [tools.web_search]
            provider = "tavily"
            api_key_env = "TAVILY_API_KEY"
            max_results = 7
            timeout_secs = 15

            [tools.lsp]
            enabled = false
            timeout_secs = 12
            max_file_bytes = 2048
            [tools.lsp.servers]
            rust = "/opt/rust-analyzer"
        "#;
        let c: Config = toml::from_str(toml).unwrap();
        assert_eq!(c.tools.web_search.provider, "tavily");
        assert_eq!(
            c.tools.web_search.api_key_env.as_deref(),
            Some("TAVILY_API_KEY")
        );
        assert_eq!(c.tools.web_search.max_results, 7);
        assert_eq!(c.tools.web_search.timeout_secs, 15);
        assert!(!c.tools.lsp.enabled);
        assert_eq!(c.tools.lsp.timeout_secs, 12);
        assert_eq!(c.tools.lsp.max_file_bytes, 2048);
        assert_eq!(
            c.tools.lsp.servers.get("rust").map(String::as_str),
            Some("/opt/rust-analyzer")
        );
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
        assert!(
            !c.verify.llm,
            "LLM verify must default OFF per cost-sensitive spec"
        );
        assert_eq!(c.verify.llm_model, None);
        assert_eq!(c.verify.llm_max_chars, 4000);
    }

    #[test]
    fn verify_config_parses_overrides() {
        let toml = "[verify]\nenabled = true\ncommands = [\"cargo check --quiet\", \"cargo test --quiet\"]\nmax_cycles = 2\nllm = true\nllm_model = \"deepseek-v4-flash\"\nllm_max_chars = 2000\n";
        let c: Config = toml::from_str(toml).unwrap();
        assert!(c.verify.enabled);
        assert_eq!(c.verify.commands.len(), 2);
        assert_eq!(c.verify.commands[0], "cargo check --quiet");
        assert_eq!(c.verify.max_cycles, 2);
        assert!(c.verify.llm);
        assert_eq!(c.verify.llm_model.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(c.verify.llm_max_chars, 2000);
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
