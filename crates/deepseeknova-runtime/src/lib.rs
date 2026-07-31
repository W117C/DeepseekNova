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

/// Role-based providers injected by callers that own a ModelRouter.
/// All fields optional; `None` falls back to legacy behaviour.
#[derive(Default)]
pub struct AgentRoleProviders {
    /// Delegate engine sub-agents (the `task` pointer).
    pub task: Option<Arc<dyn deepseeknova_provider::Provider>>,
    /// Agent L3 compaction (the `compact` pointer).
    pub compact: Option<Arc<dyn deepseeknova_provider::Provider>>,
    /// Pre-Done review gate verdict (the `quick` pointer / `review_model`).
    pub review: Option<Arc<dyn deepseeknova_provider::Provider>>,
}

/// Like [`build_agent`], but routes delegate-engine sub-agents and Agent L3
/// compaction to dedicated role providers (the `task` / `compact` model
/// pointers). Unset roles fall back to legacy behaviour.
#[allow(clippy::too_many_arguments)]
pub fn build_agent_with_role_providers(
    config: &Config,
    workspace_root: PathBuf,
    provider: Arc<dyn deepseeknova_provider::Provider>,
    roles: AgentRoleProviders,
    max_steps: usize,
    gate: Option<Arc<PermissionGate>>,
    extra_tools: Vec<Arc<dyn deepseeknova_core::Tool>>,
) -> anyhow::Result<deepseeknova_agent::Agent> {
    let security = build_security_context(config, &workspace_root)?;
    let steps = if max_steps > 0 {
        max_steps
    } else {
        config.agent.max_steps
    };

    let mut agent = deepseeknova_agent::Agent::new(Arc::clone(&provider), steps)
        .with_workspace_root(workspace_root.clone())
        .with_security(security.clone());

    if let Some(ref sp) = config.agent.system_prompt {
        agent = agent.with_system_prompt(sp.clone());
    }

    // Permission gate — opt-in. Reuse the caller-supplied (session-cached) gate
    // when given, otherwise build a fresh one per config. Caching the gate
    // across a session preserves its per-tool approval decision cache so the
    // user isn't re-prompted for the same operation every turn.
    let gate = gate.or_else(|| permission_gate_for(config, &workspace_root));
    if let Some(ref g) = gate {
        agent = agent.with_permission_gate(g.clone());
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
    // 记忆关闭时排除记忆工具（模型看不到其 schema），与 graph 同款处理。
    if !config.memory.enabled {
        disabled.insert("remember");
        disabled.insert("recall");
        disabled.insert("forget");
    }
    // 委派关闭时排除 delegate 工具。
    if !config.delegate.enabled {
        disabled.insert("delegate");
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

    // Dynamically-discovered tools (MCP, etc). Same disable-filter as built-ins;
    // their namespaced names (`mcp__server__tool`) can be toggled via overrides.
    register(&mut agent, extra_tools);

    // 句柄提升到外层，供主 agent 与子代理共享（delegate 需要）。
    let mut graph_ext: Option<deepseeknova_tools::GraphHandle> = None;
    let mut memory_ext: Option<deepseeknova_tools::MemoryHandle> = None;

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
                agent = agent.with_extension(handle.clone());
                graph_ext = Some(handle.clone());
                agent = agent.with_appended_system_prompt(GRAPH_RETRIEVAL_HINT);

                // Feed a global repo map into the agent's system prompt at run
                // start. Uses an empty personalization seed (global map); per-
                // turn personalized seeds are a future enhancement.
                // TODO(graph): personalized seeds from user input
                let budget = config.graph.repo_map_tokens;
                if budget > 0 {
                    let map_handle = handle.clone();
                    let provider: deepseeknova_agent::RepoMapProvider = Arc::new(move || {
                        map_handle
                            .lock()
                            .ok()
                            .and_then(|idx| idx.repo_map(budget, &[]).ok())
                            .filter(|s| !s.is_empty())
                    });
                    agent = agent.with_repo_map_provider(provider);
                }
            }
            Err(e) => tracing::warn!("graph index unavailable, tools will degrade: {e}"),
        }
    }

    // ── 记忆引擎：持久化、注入工具句柄、装配起点召回 + 结束沉淀 ──
    if config.memory.enabled {
        let db = workspace_root.join(&config.memory.db_path);
        match deepseeknova_core::memory::engine::MemoryEngine::open(
            &db,
            config.memory.redact_secrets,
        ) {
            Ok(engine) => {
                let handle: deepseeknova_tools::MemoryHandle = Arc::new(engine);
                agent = agent.with_extension(handle.clone());
                memory_ext = Some(handle.clone());

                // 起点召回注入（token 预算内的极简块）。
                let rp = handle.clone();
                let top_k = config.memory.recall_top_k;
                let cap_chars = config.memory.recall_inject_tokens.saturating_mul(4);
                if cap_chars > 0 {
                    let recall: deepseeknova_agent::RecallProvider =
                        Arc::new(move |query: &str| {
                            let hits = rp.recall(query, top_k).ok()?;
                            if hits.is_empty() {
                                return None;
                            }
                            let mut block = String::from("## Recalled Context\n");
                            let mut budget = cap_chars;
                            for h in &hits {
                                let snippet: String = h.entry.content.chars().take(160).collect();
                                let line = format!("- [{}] {}\n", h.entry.id, snippet);
                                if line.len() > budget {
                                    break;
                                }
                                budget -= line.len();
                                block.push_str(&line);
                            }
                            Some(block)
                        });
                    agent = agent.with_recall_provider(recall);
                }

                // 结束沉淀钩子（启发式，无 LLM）。
                let dh = handle.clone();
                let guards = deepseeknova_core::memory::engine::DistillGuards {
                    auto_learn: config.memory.auto_learn,
                    min_tool_calls: config.memory.min_tool_calls,
                    min_steps: config.memory.min_steps,
                    max_per_day: config.memory.max_distillations_per_day,
                    max_per_session: config.memory.max_distillations_per_session,
                };
                let distill: deepseeknova_agent::DistillHook = Arc::new(move |obs| {
                    if let Err(e) = dh.record_task(&obs, &guards) {
                        tracing::warn!("memory distill failed: {e}");
                    }
                });
                agent = agent.with_distill_hook(distill);

                // B3 审查计数：memory 启用时落 counters 表；关闭时 agent 内 tracing 兜底。
                if config.review.enabled {
                    let ch = handle.clone();
                    agent = agent.with_review_counter(std::sync::Arc::new(move |name: &str| {
                        let _ = ch.bump_counter(name);
                    }));
                }
            }
            Err(e) => tracing::warn!("memory engine unavailable, tools will degrade: {e}"),
        }
    }

    // ── 委派引擎：为每个预设构建受限工具集的子 Agent（共享 graph/memory 句柄）──
    // 子代理路由到独立 task provider（若提供），否则回退主 provider。
    if config.delegate.enabled {
        let delegate_provider = roles
            .task
            .as_ref()
            .map(Arc::clone)
            .unwrap_or_else(|| Arc::clone(&provider));
        let engine = build_delegate_engine(
            config,
            delegate_provider,
            &workspace_root,
            &security,
            gate.clone(),
            graph_ext.clone(),
            memory_ext.clone(),
        );
        let handle: deepseeknova_tools::DelegateHandle = engine;
        agent = agent.with_extension(handle);
    }

    // ── B2 续航：max_steps 行为 / L3 压缩 / 预算守门 ──
    agent = agent
        .with_on_max_steps(&config.agent.on_max_steps)
        .with_l3_compaction(config.agent.l3_compaction);
    if config.budget.enabled {
        agent = agent.with_budget(
            deepseeknova_agent::budget::controller::PromptBudgetController {
                max_total_tokens: config.budget.max_total_tokens,
                max_memory_tokens: config.budget.max_memory_tokens,
            },
        );
    }
    // Compact provider 优先级：调用方注入（经 router 计量）> agent.compact_model
    // 直连回退（无 router 的调用方，如 desktop 旧入口）> 不设（L3 复用主 provider）。
    if let Some(compact) = roles.compact {
        agent = agent.with_compact_provider(compact);
    } else if !config.agent.compact_model.is_empty() {
        // 直连回退：不经 CostLedger 计量；desktop 接入 router 后可移除。
        match config
            .resolve_provider_for_model(&config.agent.compact_model)
            .cloned()
        {
            Some(cfg) => {
                match deepseeknova_provider::factory::create_provider_with_model(
                    &cfg,
                    &config.agent.compact_model,
                    None,
                ) {
                    Ok(p) => agent = agent.with_compact_provider(p.into()),
                    Err(e) => tracing::warn!(
                        "compact_model '{}' unavailable ({e}); L3 will use the main provider",
                        config.agent.compact_model
                    ),
                }
            }
            None => tracing::warn!(
                "compact_model '{}' has no matching provider; L3 will use the main provider",
                config.agent.compact_model
            ),
        }
    }

    // ── B3 完成前自审（默认关）──
    if config.review.enabled {
        // 审查模型优先级：调用方注入（经 router 计量）> review_model 直连回退
        // （无 router 的调用方）> 复用主 provider。
        let review_provider: Arc<dyn deepseeknova_provider::Provider> =
            if let Some(r) = roles.review {
                r
            } else if !config.review.review_model.is_empty() {
                // 直连回退：不经 CostLedger 计量；desktop 接入 router 后可移除。
                match config
                    .resolve_provider_for_model(&config.review.review_model)
                    .cloned()
                {
                    Some(cfg) => {
                        match deepseeknova_provider::factory::create_provider_with_model(
                            &cfg,
                            &config.review.review_model,
                            None,
                        ) {
                            Ok(p) => p.into(),
                            Err(e) => {
                                tracing::warn!(
                                    "review_model '{}' unavailable ({e}); using main provider",
                                    config.review.review_model
                                );
                                provider.clone()
                            }
                        }
                    }
                    None => {
                        tracing::warn!(
                            "review_model '{}' has no matching provider; using main provider",
                            config.review.review_model
                        );
                        provider.clone()
                    }
                }
            } else {
                provider.clone()
            };
        agent = agent.with_review(
            review_provider,
            config.review.diff_cap_tokens,
            config.review.max_cycles,
        );
    }

    Ok(agent)
}

/// Like [`build_agent`], but routes the delegate engine's sub-agents to a
/// dedicated `task` provider (the `task` model pointer). `None` falls back
/// to the main provider — identical to [`build_agent`].
#[allow(clippy::too_many_arguments)]
pub fn build_agent_with_task_provider(
    config: &Config,
    workspace_root: PathBuf,
    provider: Arc<dyn deepseeknova_provider::Provider>,
    task_provider: Option<Arc<dyn deepseeknova_provider::Provider>>,
    max_steps: usize,
    gate: Option<Arc<PermissionGate>>,
    extra_tools: Vec<Arc<dyn deepseeknova_core::Tool>>,
) -> anyhow::Result<deepseeknova_agent::Agent> {
    build_agent_with_role_providers(
        config,
        workspace_root,
        provider,
        AgentRoleProviders {
            task: task_provider,
            ..Default::default()
        },
        max_steps,
        gate,
        extra_tools,
    )
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
///
/// `extra_tools` are registered after the built-in tools and pass through the
/// same `config.tools.overrides` disable filter. Callers use it to inject
/// dynamically-discovered tools (e.g. MCP tools from [`discover_mcp_tools`]);
/// pass an empty vec when there are none.
pub fn build_agent(
    config: &Config,
    workspace_root: PathBuf,
    provider: Arc<dyn deepseeknova_provider::Provider>,
    max_steps: usize,
    gate: Option<Arc<PermissionGate>>,
    extra_tools: Vec<Arc<dyn deepseeknova_core::Tool>>,
) -> anyhow::Result<deepseeknova_agent::Agent> {
    build_agent_with_task_provider(
        config,
        workspace_root,
        provider,
        None,
        max_steps,
        gate,
        extra_tools,
    )
}

/// Connect to every enabled MCP server in `config` and return their tools,
/// ready to hand to [`build_agent`] as `extra_tools`.
///
/// This is the config-level entry point (contrast with
/// [`deepseeknova_mcp::adapter::discover_mcp_tools`], which lists tools for a
/// single already-connected server). It never fails: connection or listing
/// errors are logged and that server is skipped. An empty `mcp_servers` list
/// returns an empty vec without any I/O.
///
/// The returned tools own their transport connections (via `Arc`), so the
/// connections — and, for stdio servers, the child processes (`kill_on_drop`)
/// — live exactly as long as the tools are held by the agent.
pub async fn discover_mcp_tools(config: &Config) -> Vec<Arc<dyn deepseeknova_core::Tool>> {
    if config.mcp_servers.is_empty() {
        return Vec::new();
    }

    let discovered =
        deepseeknova_mcp::discover_and_connect(config, std::time::Duration::from_secs(30)).await;

    // List tools per server concurrently; a slow or broken `tools/list` on one
    // server must not stall the others.
    let listings = discovered.into_iter().map(|server| async move {
        let client = Arc::new(deepseeknova_mcp::McpClient::from_connection(
            server.connection,
        ));
        match deepseeknova_mcp::adapter::discover_mcp_tools(&server.name, client).await {
            Ok(tools) => tools,
            Err(e) => {
                tracing::warn!("MCP server '{}' tools/list failed: {e}", server.name);
                Vec::new()
            }
        }
    });

    futures::future::join_all(listings)
        .await
        .into_iter()
        .flatten()
        .collect()
}

/// 合并内置委派预设与 `config.delegate.agents` 覆盖（按 name 匹配覆盖字段，
/// 未匹配则新增）。供委派引擎与 coordinator 的 SubAgentRunner 共用。
fn merged_delegate_presets(config: &Config) -> Vec<deepseeknova_agent::DelegatePreset> {
    let mut presets = deepseeknova_agent::builtin_presets();
    for ov in &config.delegate.agents {
        if let Some(p) = presets.iter_mut().find(|p| p.name == ov.name) {
            if let Some(sp) = &ov.system_prompt {
                p.system_prompt = sp.clone();
            }
            if let Some(tools) = &ov.tools {
                p.tools = tools.clone();
            }
            if let Some(ms) = ov.max_steps {
                p.max_steps = ms;
            }
        } else {
            presets.push(deepseeknova_agent::DelegatePreset {
                name: ov.name.clone(),
                system_prompt: ov.system_prompt.clone().unwrap_or_default(),
                tools: ov.tools.clone().unwrap_or_default(),
                max_steps: ov.max_steps.unwrap_or(10),
            });
        }
    }
    presets
}

/// 构建委派引擎：合并内置预设与配置覆盖，为每个预设造一个受限工具集的子 Agent
/// （共享主 agent 的 graph/memory 句柄与安全策略）。禁递归：剔除任何 "delegate" 工具。
#[allow(clippy::too_many_arguments)]
fn build_delegate_engine(
    config: &Config,
    provider: Arc<dyn deepseeknova_provider::Provider>,
    workspace_root: &std::path::Path,
    security: &SecurityContext,
    gate: Option<Arc<PermissionGate>>,
    graph_ext: Option<deepseeknova_tools::GraphHandle>,
    memory_ext: Option<deepseeknova_tools::MemoryHandle>,
) -> Arc<deepseeknova_agent::DelegateEngine> {
    use deepseeknova_core::Tool;

    // 子代理工具源（沿用主 agent 的沙箱选择）。
    // 注：子代理刻意不接收 MCP/extra_tools——它们只从内置工具集派生受限子集，
    // 因此 MCP 工具天然只暴露给主 agent（等价于向 build_agent 传 vec![]）。
    let base: Vec<Arc<dyn Tool>> = if config.sandbox.enabled {
        let sandbox: Arc<dyn deepseeknova_sandbox::Sandbox> =
            Arc::from(deepseeknova_sandbox::platform_sandbox_with(
                &config.sandbox.writable_paths,
                config.sandbox.allow_network,
            ));
        deepseeknova_tools::all_builtin_tools_with_sandbox(sandbox)
    } else {
        deepseeknova_tools::all_builtin_tools()
    };

    // 合并内置预设 + 配置覆盖。
    let presets = merged_delegate_presets(config);

    let mut agents: std::collections::HashMap<String, Arc<deepseeknova_agent::Agent>> =
        std::collections::HashMap::new();
    for p in &presets {
        // 禁递归：即便配置误加 "delegate" 也剔除。
        let sub_tools: Vec<Arc<dyn Tool>> = base
            .iter()
            .filter(|t| {
                let n = t.schema().name;
                n != "delegate" && p.tools.iter().any(|allow| allow == &n)
            })
            .cloned()
            .collect();
        let mut sub = deepseeknova_agent::Agent::new(Arc::clone(&provider), p.max_steps)
            .with_workspace_root(workspace_root.to_path_buf())
            .with_security(security.clone())
            .with_system_prompt(p.system_prompt.clone());
        for t in sub_tools {
            sub.register_tool(t);
        }
        if let Some(g) = &graph_ext {
            sub = sub.with_extension(g.clone());
        }
        if let Some(m) = &memory_ext {
            sub = sub.with_extension(m.clone());
        }
        if let Some(gate) = &gate {
            sub = sub.with_permission_gate(gate.clone());
        }
        agents.insert(p.name.clone(), Arc::new(sub));
    }

    Arc::new(deepseeknova_agent::DelegateEngine::new(
        agents,
        config.delegate.max_concurrent,
        config.delegate.output_cap_tokens,
    ))
}

/// Build a [`SubAgentRunner`](deepseeknova_agent::SubAgentRunner) for
/// coordinator `Delegate` actions: sub-agent turns use `task_provider` (the
/// `task` model pointer), history compaction uses `compact_provider` (the
/// `compact` pointer), falling back to the task provider when `None`.
///
/// Presets and tool restrictions mirror the delegate engine: merged builtin
/// presets + `config.delegate.agents` overrides, sandbox-aware builtin
/// tools, `"delegate"` always excluded (no recursion), no MCP tools.
pub fn build_sub_agent_runner(
    config: &Config,
    task_provider: Arc<dyn deepseeknova_provider::Provider>,
    compact_provider: Option<Arc<dyn deepseeknova_provider::Provider>>,
) -> deepseeknova_agent::SubAgentRunner {
    use deepseeknova_core::Tool;

    // 子代理工具源（与委派引擎同款：沿用沙箱选择，不接 MCP 工具）。
    let base: Vec<Arc<dyn Tool>> = if config.sandbox.enabled {
        let sandbox: Arc<dyn deepseeknova_sandbox::Sandbox> =
            Arc::from(deepseeknova_sandbox::platform_sandbox_with(
                &config.sandbox.writable_paths,
                config.sandbox.allow_network,
            ));
        deepseeknova_tools::all_builtin_tools_with_sandbox(sandbox)
    } else {
        deepseeknova_tools::all_builtin_tools()
    };

    let mut runner = deepseeknova_agent::SubAgentRunner::new(task_provider);
    for p in merged_delegate_presets(config) {
        // 禁递归：即便配置误加 "delegate" 也剔除。
        let sub_tools: Vec<Arc<dyn Tool>> = base
            .iter()
            .filter(|t| {
                let n = t.schema().name;
                n != "delegate" && p.tools.iter().any(|allow| allow == &n)
            })
            .cloned()
            .collect();
        runner.register(
            deepseeknova_agent::SubAgentConfig::new(p.name.clone(), p.system_prompt.clone())
                .with_tools(sub_tools)
                .with_max_steps(p.max_steps),
        );
    }
    if let Some(threshold) = config.agent.compaction_threshold_tokens {
        runner = runner.with_compaction_threshold(threshold);
    }
    if let Some(compact) = compact_provider {
        runner = runner.with_compact_provider(compact);
    }
    runner
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

    // --- build_sub_agent_runner (coordinator Delegate wiring) ---

    /// Counting stub: proves the compact provider (not the task provider)
    /// served the compaction request.
    struct CountingProvider {
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl deepseeknova_provider::Provider for CountingProvider {
        async fn generate(
            &self,
            _validated: deepseeknova_provider::ValidatedRequest<'_>,
        ) -> anyhow::Result<deepseeknova_core::Message> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(deepseeknova_core::Message {
                role: deepseeknova_core::Role::Assistant,
                content: "digest".into(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            })
        }
    }

    #[tokio::test]
    async fn sub_agent_runner_registers_presets_and_uses_compact_provider() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;

        let mut config = Config::default();
        // 阈值 1 token → 首步必触发压缩。
        config.agent.compaction_threshold_tokens = Some(1);

        let task = std::sync::Arc::new(stub_provider());
        let compact = std::sync::Arc::new(CountingProvider {
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let runner = build_sub_agent_runner(&config, task, Some(compact.clone()));

        let mut stream = runner
            .run_stream(deepseeknova_core::RunInput {
                prompt: "sub_agent:explorer\ngoal: investigate something long enough".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        assert!(
            compact.calls.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "compaction should go through the compact provider"
        );
    }

    #[test]
    fn sub_agent_runner_builds_with_delegate_tool_override() {
        // "delegate" 恒被过滤（与 build_delegate_engine 同款谓词）；此处验证
        // 含 delegate 覆盖的配置能安全构造。SubAgentRunner 无公开工具观测
        // 接口，过滤逻辑由 build_delegate_engine 的既有测试共同守护。
        let mut config = Config::default();
        config
            .delegate
            .agents
            .push(deepseeknova_config::DelegateAgentOverride {
                name: "explorer".into(),
                system_prompt: None,
                tools: Some(vec!["delegate".into(), "read_file".into()]),
                max_steps: None,
            });
        let task = std::sync::Arc::new(stub_provider());
        let _runner = build_sub_agent_runner(&config, task, None);
    }

    #[tokio::test]
    async fn build_agent_wires_graph_when_enabled() {
        let mut config = Config::default();
        config.graph.enabled = true;
        let root = std::env::temp_dir().join(format!("dnv-graph-wire-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/x.rs"), "pub fn foo() {}\n").unwrap();

        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, root.clone(), provider, 5, None, vec![]).unwrap();
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
        let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
        assert!(!agent.tool_names().iter().any(|n| n == "search_code"));
        assert!(!agent.tool_names().iter().any(|n| n == "traverse_graph"));
        assert!(!agent.tool_names().iter().any(|n| n == "retrieve_entity"));
    }

    #[tokio::test]
    async fn build_agent_registers_memory_tools_when_enabled() {
        let mut config = Config::default();
        config.memory.enabled = true;
        config.graph.enabled = false;
        let root = std::env::temp_dir().join(format!("dnv-mem-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, root.clone(), provider, 5, None, vec![]).unwrap();
        let names = agent.tool_names();
        assert!(names.iter().any(|n| n == "recall"));
        assert!(names.iter().any(|n| n == "remember"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn build_agent_skips_memory_tools_when_disabled() {
        let mut config = Config::default();
        config.memory.enabled = false;
        config.graph.enabled = false;
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
        assert!(!agent.tool_names().iter().any(|n| n == "recall"));
    }

    #[tokio::test]
    async fn build_agent_registers_delegate_tool_when_enabled() {
        let mut config = Config::default();
        config.delegate.enabled = true;
        config.graph.enabled = false;
        config.memory.enabled = false;
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
        assert!(agent.tool_names().iter().any(|n| n == "delegate"));
    }

    #[tokio::test]
    async fn build_agent_with_task_provider_compiles_and_registers_delegate() {
        let mut config = Config::default();
        config.delegate.enabled = true;
        config.graph.enabled = false;
        config.memory.enabled = false;
        let main: Arc<dyn deepseeknova_provider::Provider> = Arc::new(stub_provider());
        let task: Arc<dyn deepseeknova_provider::Provider> = Arc::new(stub_provider());
        let agent = build_agent_with_task_provider(
            &config,
            std::env::temp_dir(),
            main,
            Some(task),
            0,
            None,
            vec![],
        )
        .unwrap();
        assert!(agent.tool_names().iter().any(|n| n == "delegate"));
    }

    #[test]
    fn build_agent_skips_delegate_when_disabled() {
        let mut config = Config::default();
        config.delegate.enabled = false;
        config.graph.enabled = false;
        config.memory.enabled = false;
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
        assert!(!agent.tool_names().iter().any(|n| n == "delegate"));
    }

    // A no-op tool with a caller-chosen name, used to exercise extra_tools.
    struct NamedStubTool(&'static str);

    #[async_trait::async_trait]
    impl deepseeknova_core::Tool for NamedStubTool {
        fn schema(&self) -> deepseeknova_core::types::ToolSchema {
            deepseeknova_core::types::ToolSchema {
                name: self.0.to_string(),
                description: "stub".into(),
                parameters: serde_json::json!({"type": "object"}),
            }
        }
        async fn execute(
            &self,
            _ctx: &deepseeknova_core::tool::ToolContext,
            _args: &str,
        ) -> anyhow::Result<String> {
            Ok(String::new())
        }
    }

    #[test]
    fn build_agent_registers_extra_tools() {
        let mut config = Config::default();
        config.graph.enabled = false;
        config.memory.enabled = false;
        let provider = std::sync::Arc::new(stub_provider());
        let extra: Vec<Arc<dyn deepseeknova_core::Tool>> =
            vec![Arc::new(NamedStubTool("mcp__srv__do_thing"))];
        let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, extra).unwrap();
        assert!(agent.tool_names().iter().any(|n| n == "mcp__srv__do_thing"));
    }

    #[test]
    fn build_agent_skips_extra_tool_disabled_via_overrides() {
        let mut config = Config::default();
        config.graph.enabled = false;
        config.memory.enabled = false;
        config.tools.overrides = vec![deepseeknova_config::ToolOverride {
            name: "mcp__srv__do_thing".into(),
            disabled: true,
            timeout_secs: None,
            max_file_size: None,
        }];
        let provider = std::sync::Arc::new(stub_provider());
        let extra: Vec<Arc<dyn deepseeknova_core::Tool>> =
            vec![Arc::new(NamedStubTool("mcp__srv__do_thing"))];
        let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, extra).unwrap();
        assert!(!agent.tool_names().iter().any(|n| n == "mcp__srv__do_thing"));
    }

    #[tokio::test]
    async fn discover_mcp_tools_empty_config_returns_empty() {
        let config = Config::default();
        assert!(discover_mcp_tools(&config).await.is_empty());
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
    fn build_agent_applies_b2_config() {
        let mut config = Config::default();
        config.graph.enabled = false;
        config.memory.enabled = false;
        config.agent.on_max_steps = "error".into();
        config.agent.l3_compaction = false;
        config.budget.enabled = false;
        let provider = std::sync::Arc::new(stub_provider());
        // 只验证可构建不 panic（字段私有，行为断言在 agent 侧已覆盖）。
        let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
        let _ = agent;
    }

    #[test]
    fn build_agent_with_review_enabled_constructs() {
        let mut config = Config::default();
        config.graph.enabled = false;
        config.memory.enabled = false;
        config.review.enabled = true; // review_model 空 → 复用主 provider
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
        let _ = agent;
    }

    #[test]
    fn role_providers_review_injection_wins_over_review_model() {
        // review 注入胜过 review_model 直连回退（同 compact 优先级语义）。
        let mut config = Config::default();
        config.graph.enabled = false;
        config.memory.enabled = false;
        config.review.enabled = true;
        config.review.review_model = "no-such-model".into();
        let main_p = std::sync::Arc::new(stub_provider());
        let review_p: std::sync::Arc<dyn deepseeknova_provider::Provider> =
            std::sync::Arc::new(stub_provider());
        let roles = AgentRoleProviders {
            review: Some(review_p),
            ..Default::default()
        };
        let agent = build_agent_with_role_providers(
            &config,
            std::env::temp_dir(),
            main_p,
            roles,
            5,
            None,
            vec![],
        )
        .unwrap();
        let _ = agent;
    }

    #[test]
    fn role_providers_compact_injection_wins_over_compact_model() {
        let mut config = Config::default();
        config.graph.enabled = false;
        config.memory.enabled = false;
        // compact_model 指向一个不存在的模型名——若直连回退被错误执行，
        // resolve 失败仅告警不报错，因此用注入路径成功构建 + 后续分支
        // 测试共同界定优先级语义。
        config.agent.compact_model = "no-such-model".into();
        let main_p = std::sync::Arc::new(stub_provider());
        let compact_p: std::sync::Arc<dyn deepseeknova_provider::Provider> =
            std::sync::Arc::new(stub_provider());
        let roles = AgentRoleProviders {
            task: None,
            compact: Some(compact_p),
            ..Default::default()
        };
        let agent = build_agent_with_role_providers(
            &config,
            std::env::temp_dir(),
            main_p,
            roles,
            5,
            None,
            vec![],
        )
        .unwrap();
        let _ = agent; // 注入路径构建成功；Agent 侧字段私有，行为由 agent crate 测试覆盖
    }

    #[test]
    fn role_providers_default_falls_back_to_compact_model_path() {
        // roles 全 None + compact_model 非空 → 走 B2 直连回退（构建不 panic，
        // 解析失败仅告警）。与旧 build_agent 行为等价。
        let mut config = Config::default();
        config.graph.enabled = false;
        config.memory.enabled = false;
        config.agent.compact_model = "no-such-model".into();
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
        let _ = agent;
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
