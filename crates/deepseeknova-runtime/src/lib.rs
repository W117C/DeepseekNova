//! # Runtime — Composition root
//!
//! Wires together all DeepseekNova subsystems: registry, context, event bus,
//! permission, security, and LLM provider into a ready-to-use agent runtime.

use std::path::PathBuf;
use std::sync::Arc;

use deepseeknova_config::Config;
use deepseeknova_context::ContextProvider;
use deepseeknova_core::registry::RegistryHub;
use deepseeknova_core::runner::{RunEventStream, RunInput, Runner};
use deepseeknova_event::EventBus;
use deepseeknova_permission::{Decision, PermissionGate, Policy};
use deepseeknova_security::audit::TracingAuditLogger;
use deepseeknova_security::capability::Capability;
use deepseeknova_security::context::SecurityContext;
use deepseeknova_security::limits::ResourceLimits;
use deepseeknova_security::policy::SecurityPolicy;

/// Retrieval-strategy hint appended to the system prompt when the code graph
/// is enabled, steering the model toward graph tools over brute-force grep.
const GRAPH_RETRIEVAL_HINT: &str = "\n\n## 代码检索策略\n\
定位代码时优先使用图检索工具，避免全片 grep 或整文件读取：\n\
1. `search_code` 按符号名/关键词定位候选实体；\n\
2. `traverse_graph` 查看调用者/被调用者关系；\n\
3. `retrieve_entity`（view=skeleton）看骨架，确认目标后再 view=full 或 read_file 取实现。";

/// Runtime is the composition root. It wires registry, context, events,
/// and permission together. Agent, Planner, SubAgent, Server all share
/// one Runtime.
pub struct Runtime {
    pub registry: Arc<std::sync::RwLock<RegistryHub>>,
    pub context: Arc<dyn ContextProvider>,
    pub events: Arc<EventBus>,
    pub permission: Arc<PermissionGate>,
    pub config: Arc<Config>,
}

impl Runtime {
    /// Create a Runtime with a given context provider.
    pub fn new(config: Config, context: Arc<dyn ContextProvider>) -> anyhow::Result<Self> {
        let permission = build_permission_gate(&config);

        Ok(Self {
            registry: Arc::new(std::sync::RwLock::new(RegistryHub::new())),
            context,
            events: Arc::new(EventBus::new(256)),
            permission: Arc::new(permission),
            config: Arc::new(config),
        })
    }

    /// Execute a Runner and return a stream of events.
    /// Events emitted during execution are published on the shared EventBus.
    pub async fn run(
        &self,
        runner: &dyn Runner,
        input: RunInput,
    ) -> anyhow::Result<RunEventStream> {
        self.events
            .publish(deepseeknova_event::AgentEvent::ModelStarted {
                provider: "default".to_string(),
                model: input
                    .model_override
                    .clone()
                    .unwrap_or_else(|| "default".to_string()),
            });

        runner.run_stream(input).await
    }

    /// Check whether a tool call is allowed by the permission policy.
    pub fn check_permission(&self, tool: &dyn deepseeknova_core::Tool, args: &str) -> Decision {
        self.permission.check(tool, args)
    }
}

/// Build a PermissionGate from Config.
pub fn build_permission_gate(config: &Config) -> PermissionGate {
    let mut allow = Vec::new();
    let mut ask = Vec::new();
    let mut deny = Vec::new();

    for rule in &config.permissions.rules {
        let r = if let Some(ref subject) = rule.subject {
            deepseeknova_permission::Rule::with_subject(&rule.tool, subject)
        } else {
            deepseeknova_permission::Rule::new(&rule.tool)
        };

        match rule.mode {
            deepseeknova_config::PermissionMode::Allow => allow.push(r),
            deepseeknova_config::PermissionMode::Ask => ask.push(r),
            deepseeknova_config::PermissionMode::Deny => deny.push(r),
        }
    }

    let mode = match config.permissions.default_mode {
        deepseeknova_config::PermissionMode::Allow => Decision::Allow,
        deepseeknova_config::PermissionMode::Ask => Decision::Ask,
        deepseeknova_config::PermissionMode::Deny => Decision::Deny,
    };

    let gate = PermissionGate::new(Policy {
        mode,
        allow,
        ask,
        deny,
    });
    // 可选速率限制：滚动一分钟内超出上限的工具调用直接 Deny。
    match config.permissions.rate_limit_per_minute {
        Some(limit) => gate.with_rate_limit(limit),
        None => gate,
    }
}

/// Return an `Arc<PermissionGate>` when permission enforcement is enabled in
/// config (pinned to `workspace_root`), otherwise `None`. Shared by the agent
/// builder and the CLI coordinator so gate activation stays consistent.
pub fn permission_gate_for(
    config: &Config,
    workspace_root: &std::path::Path,
) -> Option<Arc<PermissionGate>> {
    if config.permissions.enabled {
        Some(Arc::new(
            build_permission_gate(config).with_workspace_root(workspace_root.to_path_buf()),
        ))
    } else {
        None
    }
}

/// Parse a capability name (case-insensitive) from config.
fn parse_capability(raw: &str) -> Option<Capability> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "file_read" | "fileread" => Some(Capability::FileRead),
        "file_write" | "filewrite" => Some(Capability::FileWrite),
        "command_execute" | "commandexecute" => Some(Capability::CommandExecute),
        "network_access" | "networkaccess" => Some(Capability::NetworkAccess),
        "mcp_invoke" | "mcpinvoke" => Some(Capability::McpInvoke),
        "memory_read" | "memoryread" => Some(Capability::MemoryRead),
        "memory_write" | "memorywrite" => Some(Capability::MemoryWrite),
        _ => None,
    }
}

/// Build a [`SecurityContext`] from the `[security]` section of [`Config`].
///
/// The `workspace_root` is always added to the allow-list so builtin file
/// tools can operate inside the project. When `config.security` is all
/// defaults this returns a context equivalent to
/// [`SecurityContext::with_safe_defaults()`] but with the workspace root
/// pinned to `workspace_root`.
pub fn build_security_context(
    config: &Config,
    workspace_root: &std::path::Path,
) -> anyhow::Result<SecurityContext> {
    let sec = &config.security;

    // Capabilities: start from safe defaults, then disable configured ones.
    let mut capabilities = std::collections::HashSet::new();
    capabilities.insert(Capability::FileRead);
    capabilities.insert(Capability::FileWrite);
    capabilities.insert(Capability::CommandExecute);
    capabilities.insert(Capability::NetworkAccess);
    capabilities.insert(Capability::McpInvoke);
    capabilities.insert(Capability::MemoryRead);
    capabilities.insert(Capability::MemoryWrite);
    for raw in &sec.disabled_capabilities {
        if let Some(cap) = parse_capability(raw) {
            capabilities.remove(&cap);
        }
    }

    // Paths: workspace root is always allowed; merge user allow/deny lists.
    let mut allowed_paths = vec![workspace_root.to_path_buf()];
    for p in &sec.allowed_paths {
        allowed_paths.push(PathBuf::from(p));
    }
    let denied_paths = sec.denied_paths.iter().map(PathBuf::from).collect();

    let policy = SecurityPolicy {
        allowed_paths,
        denied_paths,
        allowed_commands: sec.allowed_commands.clone(),
        allowed_domains: sec.allowed_domains.clone(),
    };

    // Resource limits: start from defaults, override where configured.
    let mut limits = ResourceLimits::default();
    let cfg = &sec.limits;
    if let Some(v) = cfg.max_files {
        limits.max_files = v;
    }
    if let Some(v) = cfg.max_file_size {
        limits.max_file_size = v;
    }
    if let Some(v) = cfg.max_total_read_bytes {
        limits.max_total_read_bytes = v;
    }
    if let Some(v) = cfg.max_execution_time_secs {
        limits.max_execution_time = std::time::Duration::from_secs(v);
    }
    if let Some(v) = cfg.max_output_bytes {
        limits.max_output_bytes = v;
    }
    if let Some(v) = cfg.max_tool_calls {
        limits.max_tool_calls = v;
    }

    Ok(SecurityContext {
        capabilities,
        limits,
        policy,
        audit: Arc::new(TracingAuditLogger),
    })
}

/// Build a fully-wired [`deepseeknova_agent::Agent`] from config.
///
/// Single composition point for the security dual-layer, shared by CLI and
/// desktop so their wiring can't drift:
/// - always injects the [`SecurityContext`] (capabilities, path confinement,
///   resource limits);
/// - attaches the [`PermissionGate`] only when `config.permissions.enabled`;
/// - wires the shell tool to the OS sandbox only when `config.sandbox.enabled`
///   (otherwise `NoOpSandbox`), so activation is opt-in and Windows/CI stay on
///   the no-isolation path by default.
///
/// Callers add frontend-specific pieces (conversation history, approval
/// responder) on the returned agent via its builder methods.
pub fn build_agent(
    config: &Config,
    workspace_root: PathBuf,
    provider: Arc<dyn deepseeknova_provider::Provider>,
    max_steps: usize,
    gate: Option<Arc<PermissionGate>>,
) -> anyhow::Result<deepseeknova_agent::Agent> {
    let security = build_security_context(config, &workspace_root)?;
    let steps = if max_steps > 0 {
        max_steps
    } else {
        config.agent.max_steps
    };

    let mut agent = deepseeknova_agent::Agent::new(provider, steps)
        .with_workspace_root(workspace_root.clone())
        .with_security(security);

    if let Some(ref sp) = config.agent.system_prompt {
        agent = agent.with_system_prompt(sp.clone());
    }

    // Permission gate — opt-in. Reuse the caller-supplied (session-cached) gate
    // when given, otherwise build a fresh one per config. Caching the gate
    // across a session preserves its per-tool approval decision cache so the
    // user isn't re-prompted for the same operation every turn.
    let gate = gate.or_else(|| permission_gate_for(config, &workspace_root));
    if let Some(gate) = gate {
        agent = agent.with_permission_gate(gate);
    }

    // Tools — sandboxed shell only when explicitly enabled. Tools disabled via
    // `config.tools.overrides` (e.g. the desktop settings toggles) are skipped
    // at registration so the model never sees their schemas.
    let mut disabled: std::collections::HashSet<&str> = config
        .tools
        .overrides
        .iter()
        .filter(|o| o.disabled)
        .map(|o| o.name.as_str())
        .collect();
    // Graph retrieval tools only exist when the code graph is enabled. When
    // disabled, exclude them from registration so the model never sees their
    // schemas (they'd otherwise degrade with a "graph unavailable" message).
    if !config.graph.enabled {
        disabled.insert("search_code");
        disabled.insert("traverse_graph");
        disabled.insert("retrieve_entity");
    }
    let register = |agent: &mut deepseeknova_agent::Agent,
                    tools: Vec<Arc<dyn deepseeknova_core::Tool>>| {
        for tool in tools {
            if disabled.contains(tool.schema().name.as_str()) {
                continue;
            }
            agent.register_tool(tool);
        }
    };
    if config.sandbox.enabled {
        let sandbox: Arc<dyn deepseeknova_sandbox::Sandbox> =
            Arc::from(deepseeknova_sandbox::platform_sandbox_with(
                &config.sandbox.writable_paths,
                config.sandbox.allow_network,
            ));
        register(
            &mut agent,
            deepseeknova_tools::all_builtin_tools_with_sandbox(sandbox),
        );
    } else {
        register(&mut agent, deepseeknova_tools::all_builtin_tools());
    }

    // ── 代码图：可选、后台构建、注入检索工具句柄与检索策略提示 ──
    // Open the on-disk index synchronously (cheap: just opens SQLite), then
    // refresh it on a blocking thread so the expensive tree-sitter parse never
    // stalls build_agent's return or the tokio worker pool. A refresh failure
    // only warns — the agent still runs, graph tools just degrade gracefully.
    if config.graph.enabled {
        match deepseeknova_graph::GraphIndex::open(&workspace_root, config.graph.max_file_size) {
            Ok(index) => {
                let handle: deepseeknova_tools::GraphHandle =
                    Arc::new(std::sync::Mutex::new(index));
                let bg = handle.clone();
                tokio::task::spawn_blocking(move || match bg.lock() {
                    Ok(mut idx) => {
                        if let Err(e) = idx.refresh() {
                            tracing::warn!("graph index refresh failed: {e}");
                        }
                    }
                    Err(_) => tracing::warn!("graph index lock poisoned during refresh"),
                });
                agent = agent.with_extension(handle);
                agent = agent.with_appended_system_prompt(GRAPH_RETRIEVAL_HINT);
            }
            Err(e) => tracing::warn!("graph index unavailable, tools will degrade: {e}"),
        }
    }

    Ok(agent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_config::Config;
    use deepseeknova_context::ContextEngine;
    use deepseeknova_security::capability::Capability;

    // Minimal Provider stub: never actually called by these tests (they only
    // assert on the synchronously-registered tool set), but build_agent needs
    // a concrete provider to construct the agent.
    struct StubProvider;

    #[async_trait::async_trait]
    impl deepseeknova_provider::Provider for StubProvider {
        async fn generate(
            &self,
            _validated: deepseeknova_provider::ValidatedRequest<'_>,
        ) -> anyhow::Result<deepseeknova_core::Message> {
            Ok(deepseeknova_core::Message {
                role: deepseeknova_core::Role::Assistant,
                content: String::new(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            })
        }
    }

    fn stub_provider() -> StubProvider {
        StubProvider
    }

    #[tokio::test]
    async fn build_agent_wires_graph_when_enabled() {
        let mut config = Config::default();
        config.graph.enabled = true;
        let root = std::env::temp_dir().join(format!("dnv-graph-wire-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/x.rs"), "pub fn foo() {}\n").unwrap();

        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, root.clone(), provider, 5, None).unwrap();
        let names = agent.tool_names();
        assert!(names.iter().any(|n| n == "search_code"));
        assert!(names.iter().any(|n| n == "traverse_graph"));
        assert!(names.iter().any(|n| n == "retrieve_entity"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn build_agent_skips_graph_when_disabled() {
        let mut config = Config::default();
        config.graph.enabled = false;
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None).unwrap();
        assert!(!agent.tool_names().iter().any(|n| n == "search_code"));
        assert!(!agent.tool_names().iter().any(|n| n == "traverse_graph"));
        assert!(!agent.tool_names().iter().any(|n| n == "retrieve_entity"));
    }

    #[test]
    fn build_security_context_default_grants_all_capabilities() {
        let config = Config::default();
        let root =
            std::env::temp_dir().join(format!("deepseeknova-sec-default-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();

        let ctx = super::build_security_context(&config, &root).unwrap();
        for cap in [
            Capability::FileRead,
            Capability::FileWrite,
            Capability::CommandExecute,
            Capability::NetworkAccess,
            Capability::McpInvoke,
            Capability::MemoryRead,
            Capability::MemoryWrite,
        ] {
            assert!(
                ctx.capabilities.contains(&cap),
                "expected {cap:?} granted by default"
            );
        }
        // 工作区根必须自动出现在允许路径里（即使配置无 allowed_paths）。
        assert!(ctx.policy.allowed_paths.iter().any(|p| p == &root));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn build_security_context_honors_disabled_capabilities_and_lists() {
        let mut config = Config::default();
        config.security.disabled_capabilities =
            vec!["command_execute".into(), "network_access".into()];
        config.security.allowed_commands = vec!["git".into()];
        config.security.allowed_domains = vec!["api.github.com".into()];
        config.security.denied_paths = vec!["/tmp/build/secret".into()];

        let root = std::env::temp_dir().join(format!(
            "deepseeknova-sec-restricted-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let ctx = super::build_security_context(&config, &root).unwrap();
        assert!(!ctx.capabilities.contains(&Capability::CommandExecute));
        assert!(!ctx.capabilities.contains(&Capability::NetworkAccess));
        assert!(ctx.capabilities.contains(&Capability::FileRead));
        assert_eq!(ctx.policy.allowed_commands, vec!["git".to_string()]);
        assert_eq!(
            ctx.policy.allowed_domains,
            vec!["api.github.com".to_string()]
        );
        assert!(ctx
            .policy
            .denied_paths
            .iter()
            .any(|p| p.to_string_lossy().contains("secret")));
        // 工作区根 join 在用户 allowed_paths 之前。
        assert!(ctx.policy.allowed_paths.first().unwrap() == &root);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn build_security_context_applies_resource_limits() {
        let mut config = Config::default();
        config.security.limits.max_files = Some(7);
        config.security.limits.max_execution_time_secs = Some(120);
        config.security.limits.max_output_bytes = Some(1024);

        let root =
            std::env::temp_dir().join(format!("deepseeknova-sec-limits-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();

        let ctx = super::build_security_context(&config, &root).unwrap();
        assert_eq!(ctx.limits.max_files, 7);
        assert_eq!(
            ctx.limits.max_execution_time,
            std::time::Duration::from_secs(120)
        );
        assert_eq!(ctx.limits.max_output_bytes, 1024);
        // 未覆盖的限额保留默认值。
        assert_eq!(ctx.limits.max_tool_calls, 100);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn runtime_builds_with_default_config() {
        let config = Config::default();
        // Use a temp dir to avoid scanning the full project tree
        let dir = std::env::temp_dir().join(format!("deepseeknova-rt-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let context = ContextEngine::new(dir.clone()).unwrap();
        let context: Arc<dyn ContextProvider> = Arc::new(context);

        let runtime = Runtime::new(config, context).unwrap();
        assert_eq!(runtime.events.receiver_count(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
