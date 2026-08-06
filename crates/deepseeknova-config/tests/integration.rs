//! Integration tests for the Config system — TOML roundtrip, layered merge,
//! and environment variable overrides.

use deepseeknova_config::*;
use tempfile::TempDir;

#[test]
fn default_config_roundtrip() {
    let cfg = Config::default();
    let toml_str = toml::to_string(&cfg).unwrap();
    let parsed: Config = toml::from_str(&toml_str).unwrap();
    assert!(parsed.default_model.is_none());
    assert_eq!(parsed.agent.max_steps, 10);
}

#[test]
fn load_from_valid_toml() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.toml");
    std::fs::write(
        &path,
        r#"
default_model = "deepseek-v4-flash"
default_max_steps = 15

[[providers]]
name = "deepseek"
kind = "openai"
base_url = "https://api.deepseek.com"
model = "deepseek-v4-flash"
api_key_env = "DEEPSEEK_API_KEY"

[agent]
max_steps = 20
plan_mode_default = true

[permissions]
default_mode = "allow"

[[permissions.rules]]
tool = "shell"
mode = "ask"
"#,
    )
    .unwrap();

    let cfg = Config::load_from_file(&path).unwrap();
    assert_eq!(cfg.default_model.as_deref(), Some("deepseek-v4-flash"));
    assert_eq!(cfg.agent.max_steps, 20);
    assert!(cfg.agent.plan_mode_default);
    assert_eq!(cfg.permissions.default_mode, PermissionMode::Allow);
    assert_eq!(cfg.permissions.rules.len(), 1);
    assert_eq!(cfg.permissions.rules[0].tool, "shell");
}

#[test]
fn load_invalid_toml_errors() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bad.toml");
    std::fs::write(&path, "this is not valid toml ===!!").unwrap();

    let result = Config::load_from_file(&path);
    assert!(result.is_err());
}

#[test]
fn merge_preserves_provider_overrides() {
    let mut base = Config::default();
    base.providers.push(ProviderConfig {
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
    });

    let over = Config {
        default_model: Some("gpt-5".into()),
        agent: AgentConfig {
            max_steps: 25,
            plan_mode_default: true,
            ..Default::default()
        },
        ..Default::default()
    };

    base.merge(over);
    assert_eq!(base.default_model.as_deref(), Some("gpt-5"));
    assert_eq!(base.agent.max_steps, 25);
    assert!(base.agent.plan_mode_default);
    // Providers should be preserved when override doesn't provide any
    assert_eq!(base.providers.len(), 1);
    assert_eq!(base.providers[0].name, "deepseek");
}

#[test]
fn merge_overrides_non_default_providers() {
    let mut base = Config::default();
    base.providers.push(ProviderConfig {
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
    });

    let over = Config {
        providers: vec![ProviderConfig {
            name: "openai".into(),
            kind: "openai".into(),
            base_url: None,
            model: Some("gpt-4o".into()),
            api_key_env: Some("OPENAI_API_KEY".into()),
            api_key: None,
            timeout_secs: 60,
            max_retries: 2,
            headers: vec![],
            thinking_enabled: false,
            reasoning_effort: None,
            extra_body: None,
            context_window: None,
        }],
        ..Default::default()
    };

    base.merge(over);
    assert_eq!(base.providers.len(), 1);
    assert_eq!(base.providers[0].name, "openai");
    assert_eq!(base.providers[0].timeout_secs, 60);
}

#[test]
fn find_model_by_name() {
    let cfg = Config {
        models: vec![ModelConfig {
            name: "deepseek-v4-flash".into(),
            provider: "deepseek".into(),
            context_window: Some(65536),
            max_tokens: Some(4096),
            temperature: None,
            supports_streaming: true,
            supports_tools: true,
            supports_vision: false,
            planner_only: false,
            input_price_per_mtok: None,
            output_price_per_mtok: None,
            cache_hit_price_per_mtok: None,
        }],
        ..Config::default()
    };

    let model = cfg.find_model("deepseek-v4-flash").unwrap();
    assert_eq!(model.provider, "deepseek");
    assert_eq!(model.context_window, Some(65536));
    assert!(cfg.find_model("nonexistent").is_none());
}

#[test]
fn resolve_provider_for_model() {
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
        models: vec![ModelConfig {
            name: "deepseek-v4-flash".into(),
            provider: "deepseek".into(),
            context_window: None,
            max_tokens: None,
            temperature: None,
            supports_streaming: true,
            supports_tools: true,
            supports_vision: false,
            planner_only: false,
            input_price_per_mtok: None,
            output_price_per_mtok: None,
            cache_hit_price_per_mtok: None,
        }],
        ..Config::default()
    };

    let provider = cfg.resolve_provider_for_model("deepseek-v4-flash").unwrap();
    assert_eq!(provider.name, "deepseek");
}

#[test]
fn permission_mode_serde() {
    let json = r#""allow""#;
    let mode: PermissionMode = serde_json::from_str(json).unwrap();
    assert_eq!(mode, PermissionMode::Allow);
}

#[test]
fn sandbox_defaults_are_safe() {
    let cfg = Config::default();
    assert!(!cfg.sandbox.enabled);
    assert!(!cfg.sandbox.allow_network);
    assert!(cfg.sandbox.writable_paths.is_empty());
}

#[test]
fn mcp_server_config_serde() {
    let json = r#"
    {
        "name": "filesystem",
        "command": "npx",
        "args": ["-y", "@modelcontextprotocol/server-filesystem", "."],
        "env": [{"name": "HOME", "value": "/tmp"}],
        "enabled": true
    }
    "#;
    let srv: McpServerConfig = serde_json::from_str(json).unwrap();
    assert_eq!(srv.name, "filesystem");
    assert_eq!(srv.command, "npx");
    assert_eq!(srv.args[0], "-y");
    assert!(srv.enabled);
}

#[test]
fn telemetry_disabled_by_default() {
    let cfg = Config::default();
    assert!(!cfg.telemetry.enabled);
    assert!(cfg.telemetry.otlp_endpoint.is_none());
}

#[test]
fn telemetry_toml_parse() {
    let toml = r#"
[telemetry]
enabled = true
otlp_endpoint = "http://localhost:4317"
"#;
    let cfg: Config = toml::from_str(toml).unwrap();
    assert!(cfg.telemetry.enabled);
    assert_eq!(
        cfg.telemetry.otlp_endpoint.as_deref(),
        Some("http://localhost:4317")
    );
}

#[test]
fn telemetry_merge_overrides_endpoint() {
    let mut base = Config::default();
    base.telemetry.enabled = true;
    base.telemetry.otlp_endpoint = Some("http://base:4317".into());

    // Overlay with an endpoint: both fields overwrite.
    let mut overlay = Config::default();
    overlay.telemetry.enabled = true;
    overlay.telemetry.otlp_endpoint = Some("http://overlay:4317".into());
    base.merge(overlay);
    assert!(base.telemetry.enabled);
    assert_eq!(
        base.telemetry.otlp_endpoint.as_deref(),
        Some("http://overlay:4317")
    );

    // Overlay without endpoint: enabled overwrites, endpoint preserved.
    base.merge(Config::default());
    assert!(!base.telemetry.enabled);
    assert_eq!(
        base.telemetry.otlp_endpoint.as_deref(),
        Some("http://overlay:4317")
    );
}

// ---------------------------------------------------------------------------
// Model pointers & pricing (multi-model orchestration)
// ---------------------------------------------------------------------------

fn pointer_config_toml() -> &'static str {
    r#"
        [[providers]]
        name = "deepseek"
        kind = "openai"

        [[models]]
        name = "big"
        provider = "deepseek"
        input_price_per_mtok = 0.28
        output_price_per_mtok = 0.42

        [[models]]
        name = "small"
        provider = "deepseek"

        [model_pointers]
        main = "big"
        task = "small"
    "#
}

#[test]
fn model_pointers_parse_and_validate() {
    let cfg: deepseeknova_config::Config = toml::from_str(pointer_config_toml()).unwrap();
    assert_eq!(cfg.model_pointers.main.as_deref(), Some("big"));
    assert_eq!(cfg.model_pointers.task.as_deref(), Some("small"));
    assert_eq!(cfg.model_pointers.compact, None);
    assert_eq!(cfg.model_pointers.quick, None);
    let big = cfg.find_model("big").unwrap();
    assert_eq!(big.input_price_per_mtok, Some(0.28));
    assert_eq!(big.output_price_per_mtok, Some(0.42));
    assert_eq!(big.cache_hit_price_per_mtok, None);
    cfg.validate().unwrap();
}

#[test]
fn dangling_pointer_rejected() {
    let mut cfg: deepseeknova_config::Config = toml::from_str(pointer_config_toml()).unwrap();
    cfg.model_pointers.quick = Some("no-such-model".to_string());
    let err = cfg.validate().unwrap_err().to_string();
    assert!(
        err.contains("quick"),
        "error should name the pointer: {err}"
    );
    assert!(
        err.contains("no-such-model"),
        "error should name the model: {err}"
    );
    assert!(err.contains("big"), "error should list candidates: {err}");
}

#[test]
fn negative_price_rejected() {
    let mut cfg: deepseeknova_config::Config = toml::from_str(pointer_config_toml()).unwrap();
    cfg.models[0].input_price_per_mtok = Some(-1.0);
    assert!(cfg.validate().is_err());
}

#[test]
fn non_finite_price_rejected() {
    let mut cfg: deepseeknova_config::Config = toml::from_str(pointer_config_toml()).unwrap();
    cfg.models[0].input_price_per_mtok = Some(f64::NAN);
    assert!(cfg.validate().is_err());
    cfg.models[0].input_price_per_mtok = Some(f64::INFINITY);
    assert!(cfg.validate().is_err());
}

#[test]
fn model_pointers_merge_project_over_user() {
    let mut user: deepseeknova_config::Config = toml::from_str(pointer_config_toml()).unwrap();
    let mut project = deepseeknova_config::Config::default();
    project.model_pointers.main = Some("small".to_string());
    user.merge(project);
    // 项目层覆盖 main；未设置的 task 保留用户层的值
    assert_eq!(user.model_pointers.main.as_deref(), Some("small"));
    assert_eq!(user.model_pointers.task.as_deref(), Some("small"));
}
