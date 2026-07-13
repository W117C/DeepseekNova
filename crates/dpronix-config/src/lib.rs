//! dpronix-config — layered configuration with TOML loading and merge semantics.
//!
//! Precedence (lowest to highest):
//!   1. Hard-coded defaults
//!   2. `~/.dpronix/config.toml`  (user)
//!   3. `./dpronix.toml`          (project)
//!   4. Environment variables       (REASONIX_*)
//!   5. CLI flags                   (applied by caller)

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json;
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

    /// Agent behaviour tuning.
    #[serde(default)]
    pub agent: AgentConfig,

    /// Permission rules for tool execution.
    #[serde(default)]
    pub permissions: PermissionsConfig,

    /// Sandbox settings for shell and file tools.
    #[serde(default)]
    pub sandbox: SandboxConfig,

    /// MCP server definitions.
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
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
    #[serde(default)]
    pub thinking_enabled: bool,

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
    /// Model identifier (e.g. "deepseek-chat", "claude-sonnet-5-20251001").
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
}

fn default_true() -> bool {
    true
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
    #[serde(default)]
    pub compaction_threshold_tokens: Option<u32>,

    /// Whether to run tools concurrently when possible.
    #[serde(default = "default_true")]
    pub concurrent_tools: bool,

    /// Whether plan mode is enabled by default.
    #[serde(default)]
    pub plan_mode_default: bool,
}

fn default_max_steps() -> usize {
    10
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            system_prompt: None,
            max_steps: default_max_steps(),
            compaction_threshold_tokens: None,
            concurrent_tools: true,
            plan_mode_default: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionsConfig {
    /// Default mode for write tools when no rule matches.
    #[serde(default)]
    pub default_mode: PermissionMode,

    /// Rules ordered by priority. First match wins.
    #[serde(default)]
    pub rules: Vec<PermissionRule>,
}

impl Default for PermissionsConfig {
    fn default() -> Self {
        Self {
            default_mode: PermissionMode::Ask,
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

        // Layer 1: user-global config (~/.dpronix/config.toml)
        if let Some(user_path) = user_config_path() {
            if user_path.exists() {
                let user = Self::load_from_file(&user_path)?;
                config.merge(user);
            }
        }

        // Layer 2: project-local config (./dpronix.toml)
        let project_path = PathBuf::from("dpronix.toml");
        if project_path.exists() {
            let project = Self::load_from_file(&project_path)?;
            config.merge(project);
        }

        // Layer 3: environment variables
        config.apply_env_overrides();

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
    }

    /// Apply REASONIX_* environment variable overrides.
    fn apply_env_overrides(&mut self) {
        if let Ok(val) = std::env::var("REASONIX_MODEL") {
            self.default_model = Some(val);
        }
        if let Ok(val) = std::env::var("REASONIX_MAX_STEPS") {
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
    }
}

impl PermissionsConfig {
    fn merge(&mut self, other: PermissionsConfig) {
        self.default_mode = other.default_mode;
        if !other.rules.is_empty() {
            self.rules = other.rules;
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn user_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".dpronix").join("config.toml"))
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
                model: Some("deepseek-chat".into()),
                api_key_env: Some("DEEPSEEK_API_KEY".into()),
                api_key: None,
                timeout_secs: 120,
                max_retries: 3,
                headers: vec![],
                thinking_enabled: false,
                extra_body: None,
            }],
            ..Config::default()
        };

        assert!(cfg.find_provider("deepseek").is_some());
        assert!(cfg.find_provider("nonexistent").is_none());
    }
}
