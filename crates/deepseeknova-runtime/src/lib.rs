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
use deepseeknova_security::quality::redact_secrets;

/// Retrieval-strategy hint appended to the system prompt when the code graph
/// is enabled, steering the model toward graph tools over brute-force grep.
/// English to stay consistent with the unified system-prompt family.
const GRAPH_RETRIEVAL_HINT: &str = "\n\n## Code Retrieval Strategy\n\
When locating code, prefer graph retrieval tools over blanket grep or \
whole-file reads:\n\
1. `search_code` to locate candidate symbols/entities by name or keyword;\n\
2. `traverse_graph` to inspect callers/callees;\n\
3. `retrieve_entity` (view=skeleton) to inspect structure, then view=full or \
read_file once the target is confirmed;\n\
4. `trace_code` to follow multi-hop call chains, including dynamic dispatch;\n\
5. `impact_code` to estimate the blast radius of a refactor;\n\
6. `explore_code` to read several entities' source grouped by file;\n\
7. `deps_code` to inspect imports and external dependencies.";

/// 设计 C：运行时「会话边界」序号。每次 `build_agent` 递增一次，用作
/// skill `record_use` 的会话 id——每次构造 = 新会话，跨 build 的 recall
/// 命中推进 `sessions_seen`（`verified` → `active` 的跨会话存活判据）。
/// recall 闭包签名（`Fn(&str)`）无 session 参数，故由运行时持有。
static SKILL_SESSION_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A3 从用户输入提取 repo map 个性化 seeds：标识符 token（≥3 字符、
/// 去停用词、去重、上限 8），用于对图节点做 personalized PageRank。
fn repo_map_seeds(query: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "and", "for", "with", "this", "that", "from", "into", "file", "code", "please",
        "help", "need", "want", "make", "fix", "add", "new", "use", "using", "should", "could",
        "would", "about", "your", "you", "our", "are", "not", "but", "can", "has", "have", "how",
        "what", "why", "where", "when", "which", "there", "their", "these", "those", "also",
        "then", "than", "will", "was", "were", "been", "being", "tell", "explain", "write",
        "build", "check", "review", "test", "run", "show", "list",
    ];
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for token in query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| t.len() >= 3)
    {
        if STOP.contains(&token.to_lowercase().as_str()) {
            continue;
        }
        if seen.insert(token.to_lowercase()) {
            out.push(token.to_string());
        }
        if out.len() >= 8 {
            break;
        }
    }
    out
}

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
    pub fn check_permission(
        &self,
        tool: &dyn deepseeknova_core::Tool,
        args: &str,
    ) -> deepseeknova_permission::CheckVerdict {
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

/// 沙箱可写根：配置的 `writable_paths` 加上工作区根。
///
/// 工作区默认可写（与 `build_security_context` 把 workspace root 加入
/// allow-list 的语义对齐）；已显式配置时不重复添加。
fn sandbox_writable_paths(config: &Config, workspace_root: &std::path::Path) -> Vec<String> {
    let root = workspace_root.to_string_lossy().into_owned();
    let mut paths: Vec<String> = config.sandbox.writable_paths.clone();
    if !paths.iter().any(|p| p == &root) {
        paths.push(root);
    }
    paths
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
#[non_exhaustive]
pub struct AgentRoleProviders {
    /// Delegate engine sub-agents (the `task` pointer).
    pub task: Option<Arc<dyn deepseeknova_provider::Provider>>,
    /// Agent L3 compaction (the `compact` pointer).
    pub compact: Option<Arc<dyn deepseeknova_provider::Provider>>,
    /// Pre-Done review gate verdict (the `quick` pointer / `review_model`).
    pub review: Option<Arc<dyn deepseeknova_provider::Provider>>,
    /// P2.1 每步 effort 路由：机械续步（thinking off）。
    pub step_quick: Option<Arc<dyn deepseeknova_provider::Provider>>,
    /// P2.1 每步 effort 路由：首步 / 出错 / 回炉反馈（高推理）。
    pub step_high: Option<Arc<dyn deepseeknova_provider::Provider>>,
}

/// 压缩阈值推导：显式配置优先；否则 budget 启用时取 max_total_tokens/2；都没有则 None。
fn derive_compaction_threshold(config: &Config) -> Option<u32> {
    if let Some(explicit) = config.agent.compaction_threshold_tokens {
        return Some(explicit);
    }
    if config.budget.enabled {
        return Some((config.budget.max_total_tokens / 2) as u32);
    }
    None
}

/// Like [`build_agent`], but routes delegate-engine sub-agents and Agent L3
/// compaction to dedicated role providers (the `task` / `compact` model
/// pointers). Unset roles fall back to legacy behaviour.
#[allow(clippy::too_many_arguments)]
/// 构建装配完整的 Agent（安全双层 + 工具注册 + 记忆/技能/图检索等）。
///
/// `session_skills`：可选的本会话技能名收集器（`Arc<Mutex<Vec<String>>>`）。
/// 传 `Some` 时，起点召回注入侧会把**实际注入**（进入 prompt 的）技能名写入
/// 该集合，供调用方（CLI）在会话结束时汇入 [`attach_metrics_hook_with_fitness`]
/// 做 fitness `record_use`/`record_result`（任务书 P 任务 2，spec §13 #9）；
/// 传 `None` 时行为与旧版完全一致（不收集）。
pub fn build_agent_with_role_providers(
    config: &Config,
    workspace_root: PathBuf,
    provider: Arc<dyn deepseeknova_provider::Provider>,
    roles: AgentRoleProviders,
    max_steps: usize,
    gate: Option<Arc<PermissionGate>>,
    extra_tools: Vec<Arc<dyn deepseeknova_core::Tool>>,
    session_skills: Option<Arc<std::sync::Mutex<Vec<String>>>>,
) -> anyhow::Result<deepseeknova_agent::Agent> {
    // 提前克隆供 P2 段使用（task/compact/review 字段在下方会被移动）。
    let step_quick = roles.step_quick.clone();
    let step_high = roles.step_high.clone();
    let observe_provider = roles.compact.clone().or_else(|| step_quick.clone());
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
    // A1 检查点：写前快照共享管理器，跨进程持久化（CLI checkpoint list/rollback）。
    let checkpoint: Option<Arc<tokio::sync::Mutex<deepseeknova_checkpoint::CheckpointManager>>> =
        if config.checkpoint.enabled {
            let path = workspace_root.join(&config.checkpoint.path);
            let manager = match deepseeknova_checkpoint::CheckpointManager::load_from(&path) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!("checkpoint load failed ({e}); starting fresh");
                    deepseeknova_checkpoint::CheckpointManager::new()
                }
            }
            .with_persistence(path);
            Some(Arc::new(tokio::sync::Mutex::new(manager)))
        } else {
            None
        };
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
            Arc::from(deepseeknova_sandbox::platform_sandbox_tiered(
                deepseeknova_sandbox::SandboxTier::WorkspaceWrite,
                &sandbox_writable_paths(config, &workspace_root),
                config.sandbox.allow_network,
            ));
        register(
            &mut agent,
            deepseeknova_tools::all_builtin_tools_with_sandbox_and_checkpoint(
                sandbox,
                checkpoint.clone(),
            ),
        );
    } else {
        register(
            &mut agent,
            deepseeknova_tools::all_builtin_tools_with_sandbox_and_checkpoint(
                Arc::new(deepseeknova_sandbox::NoOpSandbox),
                checkpoint,
            ),
        );
    }

    // 文档检索工具（context7_docs）：常驻注册，与 web_fetch 同级；执行时由
    // NetworkAccess 能力把关，用户可用 tools.overrides 禁用。
    register(&mut agent, deepseeknova_tools::docs_tools());

    // 日常体验工具：web 搜索 + LSP 编辑后诊断。两者均只读、可经
    // tools.overrides 禁用（如 `name = "web_search"` / `name = "lsp_diagnostics"`）。
    register(
        &mut agent,
        deepseeknova_tools::web_search_tools(&config.tools),
    );
    register(
        &mut agent,
        deepseeknova_tools::lsp_diagnostics_tools(&config.tools),
    );

    // Dynamically-discovered tools (MCP, etc). Same disable-filter as built-ins;
    // their namespaced names (`mcp__server__tool`) can be toggled via overrides.
    register(&mut agent, extra_tools);

    // 句柄提升到外层，供主 agent 与子代理共享（delegate 需要）。
    let mut graph_ext: Option<deepseeknova_tools::GraphHandle> = None;
    let mut memory_ext: Option<deepseeknova_tools::MemoryHandle> = None;

    // 高级图查询工具（trace_code / impact_code / explore_code）：仅在代码图启用时
    // 注册，与三个基础图工具「禁用时不可见」的行为等价。注册点保持在 runtime，
    // 不改动 all_builtin 工具列表（其 schema 预算测试在 tools crate 内冻结）。
    if config.graph.enabled {
        register(&mut agent, deepseeknova_tools::graph_query_tools());
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
                    let provider: deepseeknova_agent::RepoMapProvider =
                        Arc::new(move |query: &str| {
                            let seeds = repo_map_seeds(query);
                            map_handle
                                .lock()
                                .ok()
                                .and_then(|idx| idx.repo_map(budget, &seeds).ok())
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
        // 语义嵌入装配（fail-open）：embedder=remote 且 key/model 可用时挂载，
        // 否则 warn 回落纯 FTS（try_memory_embedder 内部处理）。
        let memory_embedder =
            deepseeknova_provider::embeddings::try_memory_embedder(&config.memory);
        let memory_embed_model = memory_embedder
            .as_ref()
            .map(|_| config.memory.embed_model.clone());
        match deepseeknova_core::memory::engine::MemoryEngine::open_with_embedder(
            &db,
            config.memory.redact_secrets,
            memory_embedder,
            memory_embed_model,
        ) {
            Ok(engine) => {
                let handle: deepseeknova_tools::MemoryHandle = Arc::new(engine);
                // C3：注入召回排序权重扩展（tools recall 工具读取），
                // 与起点/mid-run 召回共用 `[memory] rank_lifecycle_weight`。
                agent = agent.with_extension(handle.clone());
                agent = agent.with_extension(deepseeknova_tools::MemoryRankWeight(
                    config.memory.rank_lifecycle_weight,
                ));
                memory_ext = Some(handle.clone());

                // 设计 C：技能热更新——SkillManager 装配（skill 即记忆上下文）。
                // 蒸馏 skill 写入 `<root>/.deepseeknova/skills/auto/`（与用户手写
                // skill 隔离，frontmatter 强制 `source: distill`）；会话边界
                // （每次 build_agent 新构造 = 天然重载）+ 蒸馏落盘后显式 reload，
                // 同会话后续 recall 即可注入新 skill。
                let skill_manager: Arc<
                    std::sync::Mutex<deepseeknova_core::memory::skill::SkillManager>,
                > = Arc::new(std::sync::Mutex::new(
                    deepseeknova_core::memory::skill::SkillManager::new(
                        deepseeknova_core::memory::skill::SkillExtractionConfig {
                            skill_dir: workspace_root.join(".deepseeknova/skills"),
                            // 蒸馏提取门槛与 `[memory] min_tool_calls/min_steps` 同源：
                            // 测试与用户配置才能实际调低/调高 auto/ 落盘门槛。
                            min_tool_calls: config.memory.min_tool_calls,
                            min_steps: config.memory.min_steps,
                            // 三态质量门槛与 `[memory] verify_use_threshold /
                            // active_session_threshold / max_auto_draft_skills` 同源
                            // （默认 3/3/20，对齐 skill.rs 常量）。
                            verify_use_threshold: config.memory.verify_use_threshold,
                            active_session_threshold: config.memory.active_session_threshold,
                            max_auto_draft_skills: config.memory.max_auto_draft_skills,
                        },
                    ),
                ));
                let skills_for_distill = skill_manager.clone();

                // 起点召回注入（token 预算内的极简块）。
                let rp = handle.clone();
                let top_k = config.memory.recall_top_k;
                // C3：起点召回接 `[memory] rank_lifecycle_weight`（此前硬编码默认 0.3）。
                let rank_weight = config.memory.rank_lifecycle_weight;
                let cap_chars =
                    deepseeknova_core::tokens::chars_for_tokens(config.memory.recall_inject_tokens);
                if cap_chars > 0 {
                    let skills_for_recall = skill_manager.clone();
                    // P 任务 2：技能名收集器（注入侧写入，fitness 侧消费）。
                    let skills_sink = session_skills.clone();
                    // 设计 C 三态闭环：每次 build_agent = 新会话，recall 注入
                    // 命中即计一次 use（success=true），驱动 draft → verified
                    // → active 状态迁移（阈值来自 SkillExtractionConfig）。
                    let skill_session_id = format!(
                        "rt-{}-{}",
                        std::process::id(),
                        SKILL_SESSION_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    );
                    let recall: deepseeknova_agent::RecallProvider =
                        Arc::new(move |query: &str| {
                            let hits = rp
                                .recall_with_weight(query, top_k, rank_weight)
                                .ok()
                                .unwrap_or_default();
                            let mut block = String::new();
                            let mut budget = cap_chars;
                            if !hits.is_empty() {
                                block.push_str("## Recalled Context\n");
                                for h in &hits {
                                    let snippet: String =
                                        h.entry.content.chars().take(160).collect();
                                    let line = format!("- [{}] {}\n", h.entry.id, snippet);
                                    if line.len() > budget {
                                        break;
                                    }
                                    budget -= line.len();
                                    block.push_str(&line);
                                }
                            }
                            // 设计 C：追加匹配 skill。draft 仅高匹配度注入且排最后
                            // （低优先级试用）；verified/active 常规注入。
                            if let Ok(mut sm) = skills_for_recall.lock() {
                                // 先取 owned (name, description)，结束不可变借用
                                // 后再 record_use（需要 &mut）。
                                let matched: Vec<(String, String)> = sm
                                    .find_matching_skills(query)
                                    .iter()
                                    .map(|s| {
                                        (
                                            s.frontmatter.name.clone(),
                                            s.frontmatter.description.clone(),
                                        )
                                    })
                                    .collect();
                                if !matched.is_empty() {
                                    let mut lines = String::from("## Available Skills\n");
                                    // P2 修复：只对真正写入注入内容（lines）的 skill
                                    // 计数。受 take(top_k) 与字符预算双重约束，超预算
                                    // break 后剩余的匹配项未进入 prompt，不得 record_use
                                    // （否则「匹配即计 use」会污染 draft → verified 晋升）。
                                    let mut injected: Vec<&str> = Vec::new();
                                    for (name, desc) in matched.iter().take(top_k) {
                                        let line = format!("- **{name}**: {desc}\n");
                                        if lines.len() > budget {
                                            break;
                                        }
                                        lines.push_str(&line);
                                        injected.push(name);
                                    }
                                    if lines.len() > "## Available Skills\n".len() {
                                        block.push_str(&lines);
                                    }
                                    // 三态闭环：注入即计一次 use（成功语义）。
                                    // 失败仅 warn（record_use 落盘失败不阻断注入）。
                                    // P 任务 2：把实际注入的技能名同时写入
                                    // session_skills 收集器（去重），供会话结束
                                    // fitness record_use/record_result（spec §13
                                    // #9 接线；None = 不收集，行为与旧版一致）。
                                    for name in injected {
                                        if let Err(e) =
                                            sm.record_use(name, true, Some(&skill_session_id))
                                        {
                                            tracing::warn!(
                                                "skill record_use failed for '{name}': {e}"
                                            );
                                        }
                                        if let Some(ref sink) = skills_sink {
                                            if let Ok(mut guard) = sink.lock() {
                                                if !guard.iter().any(|s| s == name) {
                                                    guard.push(name.to_string());
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            if block.is_empty() {
                                None
                            } else {
                                Some(block)
                            }
                        });
                    agent = agent.with_recall_provider(recall);

                    // P3.2/P3.3 中途检索（F1 修复：配置真实生效）：
                    // `mid_run_recall=false` 不装配；top_k / inject_tokens /
                    // require_tool_turn 全部来自配置；图索引启用时同时注入
                    // 代码图命中（mid_run_graph_top_k）。
                    if config.memory.mid_run_recall {
                        let mid_mem = handle.clone();
                        let mid_graph = graph_ext.clone();
                        let mid_top_k = config.memory.mid_run_recall_top_k;
                        let mid_graph_top_k = config.memory.mid_run_graph_top_k;
                        // C3：mid-run 召回同样接 `[memory] rank_lifecycle_weight`。
                        let mid_rank_weight = config.memory.rank_lifecycle_weight;
                        let mid_cap = deepseeknova_core::tokens::chars_for_tokens(
                            config.memory.mid_run_inject_tokens,
                        );
                        let mid: deepseeknova_agent::RecallProvider =
                            Arc::new(move |query: &str| {
                                if mid_cap == 0 {
                                    return None;
                                }
                                let mut block = String::new();
                                let mut budget = mid_cap;
                                if let Ok(hits) =
                                    mid_mem.recall_with_weight(query, mid_top_k, mid_rank_weight)
                                {
                                    let mut lines: Vec<String> = Vec::new();
                                    for h in hits {
                                        let snippet: String =
                                            h.entry.content.chars().take(120).collect();
                                        let line = format!("- [memory] {snippet}\n");
                                        if line.len() > budget {
                                            break;
                                        }
                                        lines.push(line);
                                    }
                                    if !lines.is_empty() {
                                        block.push_str("## Recalled Context\n");
                                        for l in lines {
                                            budget -= l.len();
                                            block.push_str(&l);
                                        }
                                    }
                                }
                                if let Some(ref g) = mid_graph {
                                    if let Ok(idx) = g.lock() {
                                        if let Ok(nodes) = idx.search(query, None, mid_graph_top_k)
                                        {
                                            let mut lines: Vec<String> = Vec::new();
                                            for n in nodes {
                                                let line =
                                                    format!("- [graph] {} ({})\n", n.name, n.path);
                                                if line.len() > budget {
                                                    break;
                                                }
                                                lines.push(line);
                                            }
                                            if !lines.is_empty() {
                                                block.push_str("## Graph Hits\n");
                                                for l in lines {
                                                    block.push_str(&l);
                                                }
                                            }
                                        }
                                    }
                                }
                                if block.is_empty() {
                                    None
                                } else {
                                    Some(block)
                                }
                            });
                        agent = agent
                            .with_mid_run_retrieval(mid, config.memory.mid_run_require_tool_turn);
                    }
                }

                // 结束沉淀钩子：启发式 record_task 兜底；`[memory] llm_distill`
                // 启用时另 spawn 异步 LLM 蒸馏（失败仅 warn，不阻断 run）。
                let dh = handle.clone();
                let guards = deepseeknova_core::memory::engine::DistillGuards {
                    auto_learn: config.memory.auto_learn,
                    min_tool_calls: config.memory.min_tool_calls,
                    min_steps: config.memory.min_steps,
                    max_per_day: config.memory.max_distillations_per_day,
                    max_per_session: config.memory.max_distillations_per_session,
                };
                let llm_distill_on = config.memory.llm_distill;
                let llm_distill_max_chars = config.memory.llm_distill_max_chars;
                let llm_distill_provider: Arc<dyn deepseeknova_provider::Provider> =
                    if llm_distill_on {
                        match config.memory.llm_distill_model.as_deref() {
                            Some(model) => match config.resolve_provider_for_model(model).cloned() {
                                Some(cfg) => {
                                    match deepseeknova_provider::factory::create_provider_with_model(
                                        &cfg, model, None,
                                    ) {
                                        Ok(p) => p.into(),
                                        Err(e) => {
                                            tracing::warn!(
                                                "llm_distill_model '{model}' unavailable ({e}); \
                                                     using main provider"
                                            );
                                            provider.clone()
                                        }
                                    }
                                }
                                None => {
                                    tracing::warn!(
                                        "llm_distill_model '{model}' has no matching provider; \
                                             using main provider"
                                    );
                                    provider.clone()
                                }
                            },
                            None => provider.clone(),
                        }
                    } else {
                        provider.clone()
                    };
                // 仅启用时取 Handle（同步测试无 tokio runtime，默认关闭不受影响）。
                let tokio_handle = if llm_distill_on {
                    Some(tokio::runtime::Handle::current())
                } else {
                    None
                };
                // 设计 C 三态闭环：会话边界清理超额 distill draft 的上限
                // （`[memory] max_auto_draft_skills`，默认 20，对齐 skill.rs 常量）。
                let max_auto_draft_skills = config.memory.max_auto_draft_skills;
                let distill: deepseeknova_agent::DistillHook = Arc::new(move |obs| {
                    if let Err(e) = dh.record_task(&obs, &guards) {
                        tracing::warn!("memory distill failed: {e}");
                    }
                    if let (true, Some(handle)) = (llm_distill_on, tokio_handle.as_ref()) {
                        let engine = dh.clone();
                        let llm = llm_distill_provider.clone();
                        let max_chars = llm_distill_max_chars;
                        let skills = skills_for_distill.clone();
                        let obs = obs.clone();
                        handle.spawn(async move {
                            if let Some(k) = deepseeknova_agent::memory_distill::run_llm_distill(
                                llm.as_ref(),
                                &obs,
                                max_chars,
                            )
                            .await
                            {
                                // 设计 C：skill 分支 → SkillManager auto/ 落盘
                                // （source: distill 隔离；should_extract_skill 门槛），
                                // 落盘后 reload 使同会话后续 recall 立即可注入。
                                if let Ok(mut sm) = skills.lock() {
                                    match deepseeknova_agent::memory_distill::
                                        persist_distilled_skill(&mut sm, &obs, &k)
                                    {
                                        Ok(true) => {
                                            if let Err(e) = sm.reload() {
                                                tracing::warn!(
                                                    "skill manager reload failed: {e}"
                                                );
                                            }
                                        }
                                        Ok(false) => {}
                                        Err(e) => {
                                            tracing::warn!("distilled skill persist failed: {e}")
                                        }
                                    }
                                }
                                if let Err(e) =
                                    engine.record_llm_knowledge(&k.kind, &k.title, &k.body, k.tags)
                                {
                                    tracing::warn!("llm distill store failed: {e}");
                                }
                            }
                        });
                    }
                    // 设计 C 三态闭环：会话边界（run 结束）清理超额 distill draft。
                    // 仅删 `source: distill` + `state: draft`（LRU）；用户手写、
                    // verified、active 一律豁免。失败仅 warn，不阻断 run。
                    if let Ok(mut sm) = skills_for_distill.lock() {
                        if let Err(e) = sm.prune_auto_drafts(max_auto_draft_skills) {
                            tracing::warn!("skill prune failed: {e}");
                        }
                    }
                });
                agent = agent.with_distill_hook(distill);

                // 反思教训沉淀：memory 启用且反思开时挂 LessonHook（失败仅 warn）。
                if config.agent.reflect_on_failure {
                    let lh = handle.clone();
                    agent = agent.with_lesson_hook(std::sync::Arc::new(move |lesson: String| {
                        if let Err(e) = lh.record_reflection_lesson(&lesson) {
                            tracing::warn!("reflection lesson store failed: {e}");
                        }
                    }));
                }

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
        // 归因 provider：review 指针优先（低成本判定），否则主 provider。
        let attribution_provider = roles
            .review
            .as_ref()
            .map(Arc::clone)
            .unwrap_or_else(|| Arc::clone(&provider));
        let attribution = config.attribution.enabled.then(|| {
            Arc::new(deepseeknova_agent::attribution::AttributionSettings {
                provider: attribution_provider,
                max_retries: config.attribution.max_retries,
                max_attributions: config.attribution.max_attributions,
                degrade_map: config.attribution.degrade_map.clone(),
            })
        });
        let engine = build_delegate_engine(
            config,
            delegate_provider,
            &workspace_root,
            &security,
            gate.clone(),
            graph_ext.clone(),
            memory_ext.clone(),
            attribution,
        );
        let handle: deepseeknova_tools::DelegateHandle = engine;
        agent = agent.with_extension(handle);
    }

    // ── 失败归因（B，默认关）：verify/review 达上限 Paused 前 LLM 归因，──
    // reason 附带 fix_plan 摘要（Paused 恢复可续用）；归因受硬预算约束。
    // 注意：须在 B3 review 段（move roles.review）之前完成。
    if config.attribution.enabled {
        let attribution_provider = roles
            .review
            .as_ref()
            .map(Arc::clone)
            .unwrap_or_else(|| Arc::clone(&provider));
        agent = agent.with_attribution(deepseeknova_agent::attribution::AttributionSettings {
            provider: attribution_provider,
            max_retries: config.attribution.max_retries,
            max_attributions: config.attribution.max_attributions,
            degrade_map: config.attribution.degrade_map.clone(),
        });
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
    // 压缩阈值：显式配置优先，否则由 budget 推导（lossless L1 shrink 默认开启）。
    if let Some(threshold) = derive_compaction_threshold(config) {
        agent = agent.with_compaction_threshold(Some(threshold));
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

    // ── P1 并行工具执行 + 完成前确定性验证 ──
    agent = agent.with_concurrent_tools(config.agent.concurrent_tools);
    if config.verify.enabled && (!config.verify.commands.is_empty() || config.verify.llm) {
        agent = agent.with_verify(config.verify.commands.clone(), config.verify.max_cycles);
        if config.verify.llm {
            // LLM 验证 provider：`llm_model` 可选，未配置或不可用回落 main provider。
            let verify_provider: Arc<dyn deepseeknova_provider::Provider> =
                match config.verify.llm_model.as_deref() {
                    Some(model) => match config.resolve_provider_for_model(model).cloned() {
                        Some(cfg) => {
                            match deepseeknova_provider::factory::create_provider_with_model(
                                &cfg, model, None,
                            ) {
                                Ok(p) => p.into(),
                                Err(e) => {
                                    tracing::warn!(
                                        "verify llm_model '{model}' unavailable ({e}); \
                                             using main provider"
                                    );
                                    provider.clone()
                                }
                            }
                        }
                        None => {
                            tracing::warn!(
                                "verify llm_model '{model}' has no matching provider; \
                                     using main provider"
                            );
                            provider.clone()
                        }
                    },
                    None => provider.clone(),
                };
            agent = agent.with_llm_verify(verify_provider, config.verify.llm_max_chars);
        }
    }

    // ── 反思闭环：P1 验证 / B3 审查失败回炉前显式反思（默认开；模型回落 main）──
    if config.agent.reflect_on_failure {
        let reflect_provider: Arc<dyn deepseeknova_provider::Provider> =
            match config.agent.reflect_model.as_deref() {
                Some(model) => match config.resolve_provider_for_model(model).cloned() {
                    Some(cfg) => {
                        match deepseeknova_provider::factory::create_provider_with_model(
                            &cfg, model, None,
                        ) {
                            Ok(p) => p.into(),
                            Err(e) => {
                                tracing::warn!(
                                    "reflect_model '{model}' unavailable ({e}); using main provider"
                                );
                                provider.clone()
                            }
                        }
                    }
                    None => {
                        tracing::warn!(
                            "reflect_model '{model}' has no matching provider; using main provider"
                        );
                        provider.clone()
                    }
                },
                None => provider.clone(),
            };
        agent = agent.with_reflection(reflect_provider, config.agent.reflect_max_chars);
    }

    // ── P2 高频决策经济学 ──
    agent = agent.with_tool_cache(config.agent.tool_cache);
    if config.agent.step_effort_routing {
        match (step_quick, step_high) {
            (Some(quick), Some(high)) => {
                agent = agent.with_effort_routing(quick, high);
            }
            _ => tracing::warn!(
                "step_effort_routing enabled but quick/high providers missing; \
                 falling back to the fixed main provider"
            ),
        }
    }
    if config.agent.observe_compress {
        let obs_provider = observe_provider.unwrap_or_else(|| provider.clone());
        agent = agent.with_observe_compression(
            obs_provider,
            config.agent.observe_compress_threshold_chars,
            config.agent.observe_compress_max_chars,
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
        // 既有入口不收集技能名（无 fitness 消费方），保持签名与行为不变；
        // 需要收集的调用方（CLI）直接调 build_agent_with_role_providers。
        None,
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

/// 会话效能落盘所需的成本面数据与输出目录。
pub struct MetricsSink {
    pub ledger: Arc<deepseeknova_provider::cost::CostLedger>,
    pub prices: deepseeknova_provider::cost::PriceTable,
    pub dir: PathBuf,
}

/// 留存策略：目录下 `*.json` 报告数超过 `max_reports` 时删除最旧的（按文件
/// 修改时间排序，同刻按文件名字典序兜底），只保留最新的 `max_reports` 个。
/// `*.scorecard.json` 评分卡（跨会话对比数据）不参与裁剪，永不因留存被删。
/// 匹配大小写不敏感：`X.SCORECARD.JSON` 等大写扩展名同样按评分卡排除，普通
/// 大写 `.JSON` 报告同样参与留存计数（否则留存口径会漏掉它们、目录无限累积）。
/// 目录不存在/读取失败静默跳过；删除失败仅 warn，不阻断 run。`max_reports=0`
/// 视为不清理（防御，配置层默认 100 不会走到）。
pub fn enforce_metrics_retention(dir: &std::path::Path, max_reports: usize) {
    if max_reports == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            // 只统计普通报告 json；`.scorecard.json` 是跨会话对比数据，不参与
            // 报告留存裁剪（任务质量闭环 C）。文件名统一 lowercase 后匹配，
            // 大写扩展名（`X.SCORECARD.JSON`）不会被误当普通报告裁剪或漏计。
            let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase());
            match name {
                Some(n) => n.ends_with(".json") && !n.ends_with(".scorecard.json"),
                None => false,
            }
        })
        .collect();
    if files.len() <= max_reports {
        return;
    }
    files.sort_by(|a, b| {
        let ma = std::fs::metadata(a)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        let mb = std::fs::metadata(b)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        ma.cmp(&mb).then_with(|| a.cmp(b))
    });
    let excess = files.len() - max_reports;
    for old in files.into_iter().take(excess) {
        if let Err(e) = std::fs::remove_file(&old) {
            tracing::warn!("metrics retention remove failed ({}): {e}", old.display());
        }
    }
}

/// 按配置为 Agent 挂载会话效能钩子：`[metrics] enabled=true` 时，每次 run
/// 结束生成 SessionReport（执行面 + 成本面）写入 `sink.dir`，并在其后按
/// QualitySummary 组装四维评分卡（`<session_id>.scorecard.json`，任务质量
/// 闭环 C）落盘；写入失败仅 warn，不阻断 run。落盘后执行留存策略
/// （`[metrics] max_reports`，默认 100）：报告数超上限时删除最旧报告，
/// 防止 chat 每轮落盘长期累积。`enabled=false` 时原样返回，Agent 行为零变化。
///
/// 委托 [`attach_metrics_hook_with_fitness`]（空会话技能集合 + 空 workspace，
/// 不启用 fitness 记录），保持既有签名与语义不变。
pub fn attach_metrics_hook(
    agent: deepseeknova_agent::Agent,
    config: &Config,
    sink: MetricsSink,
) -> deepseeknova_agent::Agent {
    attach_metrics_hook_with_fitness(
        agent,
        config,
        sink,
        std::path::Path::new(""),
        Arc::new(std::sync::Mutex::new(Vec::new())),
    )
}

/// [`attach_metrics_hook`] 的协议增强扩展：`[protocol] enabled=true` 且
/// `workspace_root` 非空时，会话结束（metrics hook 内）按 outcome 对
/// `session_skills`（本会话激活过的技能名）逐条调
/// [`FitnessStore::record_use`](deepseeknova_skills::fitness::FitnessStore)
/// （激活计数）与
/// [`FitnessStore::record_result`](deepseeknova_skills::fitness::FitnessStore)
/// （会话成败），并 save 到 `<workspace_root>/.deepseeknova/skills/fitness.json`
/// （协议增强设计 §5 + 任务书 P 任务 2；失败仅 warn，不阻断 run）。
///
/// outcome 判定：`stats.outcome == Some(Completed)` 记 success=true，其余
/// （PausedMaxSteps/Cancelled）记 success=false。`session_skills` 由调用方
/// （CLI）经 [`build_agent_with_role_providers`] 的注入侧收集器回填真实注入
/// 的技能名（spec §13 #9 接线完成）；集合为空 = 本会话无注入技能，优雅跳过
/// （不写文件、不 warn）。`enabled=false` 或空 workspace 时
/// 行为与 [`attach_metrics_hook`] 完全一致。
///
/// task_rate（设计 §7.1 末条）：Completed 结束在评分卡落盘前按
/// `first_pass=true` 填写；Paused/Cancelled 路径本 hook 先于诊断回调触发、
/// 失败详情尚不可知，维持保守默认（false/0），由
/// [`attach_diagnose_hook_with_ingest`] 的诊断回调按 failures 覆写。
pub fn attach_metrics_hook_with_fitness(
    agent: deepseeknova_agent::Agent,
    config: &Config,
    sink: MetricsSink,
    workspace_root: &std::path::Path,
    session_skills: Arc<std::sync::Mutex<Vec<String>>>,
) -> deepseeknova_agent::Agent {
    if !config.metrics.enabled {
        return agent;
    }
    let max_reports = config.metrics.max_reports;
    // 协议增强：fitness 仅在 `[protocol] enabled` 且提供 workspace 时启用。
    let fitness_on = config.protocol.enabled && !workspace_root.as_os_str().is_empty();
    let fitness_path = workspace_root
        .join(".deepseeknova")
        .join("skills")
        .join("fitness.json");
    let hook: deepseeknova_agent::MetricsHook = Arc::new(move |stats, summary| {
        // 任务质量闭环 C：会话 id 两份文件共用，保证
        // `<id>.json` 与 `<id>.scorecard.json` 可对账。优先用 Agent 的
        // 会话标注（Paused 事件/诊断报告同源），未标注时回退生成唯一 id。
        let session_id = summary
            .session_id
            .clone()
            .unwrap_or_else(deepseeknova_metrics::new_session_id);
        let mut card = deepseeknova_metrics::Scorecard::compute(
            &session_id,
            &stats,
            &summary.findings,
            summary.reflection_count,
            summary.review_issues,
            summary.review_passes,
        );
        // 协议增强：覆写 protocol/composite 维（Scorecard::compute 已将
        // protocol 置 1.0 占位，此处用 QualitySummary 的协议统计填真实值；
        // fill_protocol 同时重算 composite 加权均值，见 metrics 侧注释）。
        card.fill_protocol(summary.protocol_violations, summary.phase_transitions);
        // task_rate（设计 §7.1 末条）：成功结束（Completed）无诊断报告
        // （agent 侧 suppress），按 first_pass=true 填写；Paused/Cancelled
        // 路径 metrics hook 先于诊断回调触发、任务失败详情尚不可知，维持
        // compute 保守默认（false/0），由 attach_diagnose_hook_with_ingest
        // 的诊断回调按 failures 覆写真实值。
        if matches!(
            stats.outcome,
            Some(deepseeknova_metrics::RunOutcome::Completed)
        ) {
            card.fill_task_rate(true, 0);
        }
        let report = deepseeknova_metrics::SessionReport {
            session_id,
            stats: stats.clone(),
            cost: sink.ledger.report(&sink.prices),
        };
        if let Err(e) = deepseeknova_metrics::write_report(&report, &sink.dir) {
            tracing::warn!("metrics report write failed: {e}");
            return;
        }
        // 评分卡独立文件落盘；失败仅 warn，不阻断 run（与 write_report 同模式）。
        if let Err(e) = deepseeknova_metrics::write_scorecard(&card, &sink.dir) {
            tracing::warn!("metrics scorecard write failed: {e}");
            return;
        }
        // P3：落盘后按 max_reports 清理最旧报告。
        enforce_metrics_retention(&sink.dir, max_reports);

        // 协议增强：会话结束 fitness 记录（仅 protocol.enabled 时动作）。
        if fitness_on {
            let skills: Vec<String> = match session_skills.lock() {
                Ok(guard) => guard.clone(),
                Err(_) => Vec::new(),
            };
            if skills.is_empty() {
                // 本会话无注入技能（空集合 = recall 注入侧确实未注入任何
                // skill）——优雅跳过，不写文件、不 warn（spec §13 #9 接线后
                // 空集合即"无注入"的合法状态，warn 噪声已移除）。
            } else {
                let success = matches!(
                    stats.outcome,
                    Some(deepseeknova_metrics::RunOutcome::Completed)
                );
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                match deepseeknova_skills::fitness::FitnessStore::load(&fitness_path) {
                    Ok(mut store) => {
                        for name in &skills {
                            // P 任务 2：record_result（会话成败）后补 record_use
                            // （注入激活计数）——skill 本会话被注入即计一次激活，
                            // 与 recall 注入侧的 SkillManager::record_use（三态
                            // 迁移）各司其职：后者驱动 draft→verified→active，
                            // 前者驱动 fitness 库的 uses 计数（spec §13 #9）。
                            store.record_use(name, now_ms);
                            store.record_result(name, success, now_ms);
                        }
                        if let Err(e) = store.save() {
                            tracing::warn!("fitness save failed: {e}");
                        }
                    }
                    Err(e) => tracing::warn!("fitness load failed: {e}"),
                }
            }
        }
    });
    agent.with_metrics_hook(hook)
}

/// 按配置为 Agent 挂载任务质量钩子（A 阶段：ToolHook 链 + 写后策略评估）：
/// `[quality] enabled=true` 时注册内置
/// [`QualityHook`](deepseeknova_agent::quality::QualityHook)（builtin 策略，含
/// no-commit-secret / no-forbidden-path / oversized-write 三条规则）。
/// `enabled=false` 时原样返回，Agent 行为零变化。
pub fn attach_quality_hook(
    agent: deepseeknova_agent::Agent,
    config: &Config,
) -> deepseeknova_agent::Agent {
    if !config.quality.enabled {
        return agent;
    }
    let hook = deepseeknova_agent::quality::QualityHook::new(
        deepseeknova_security::quality::QualityPolicy::builtin(),
    );
    agent.with_tool_hook(Arc::new(hook))
}

/// 诊断报告留存上限：对齐 `[metrics] max_reports` 配置默认值（100）。诊断
/// 子目录与 metrics 目录共用同一留存语义（`enforce_metrics_retention`）。
const DIAGNOSE_RETENTION_MAX: usize = 100;

/// 按目录为 Agent 挂载失败诊断钩子（任务质量闭环 B 阶段）：run 以非
/// success 结束（Paused/failed）时，回调闭包在 `<dir>/diagnose/` 子目录
/// （不存在则建）写 `<session_id>.json`，并复用 [`enforce_metrics_retention`]
/// 对诊断子目录执行留存（上限对齐 `[metrics] max_reports` 默认值 100）。
/// 无条件装配（低风险旁路）：写入/留存失败仅 warn，不阻断 run；成功结束
/// 不产生任何文件。
///
/// 委托 [`attach_diagnose_hook_with_ingest`]（`None` 配置 = 不启用失败模式
/// 聚类），保持既有签名与语义不变。
pub fn attach_diagnose_hook(
    agent: deepseeknova_agent::Agent,
    dir: PathBuf,
) -> deepseeknova_agent::Agent {
    attach_diagnose_hook_with_ingest(agent, dir, None, std::path::Path::new(""))
}

/// task_rate 回填决策（设计 §7.1 末条 + P-L2）：仅当诊断报告 `failures`
/// **非空**（失败型会话）时覆写评分卡 `first_pass=false` 与
/// `retry_rounds=失败条数`；零失败报告（如 Cancelled/unverified 无工具失败
/// 详情）**不覆写**，保持 metrics hook 已填的值（非 Completed 的保守
/// false/0），避免零失败会话被误标 first_pass=true。评分卡缺失/不可解析时
/// 静默跳过（metrics 未启用属正常路径）；IO 失败仅 warn，不阻断 run。
fn backfill_scorecard_task_rate(
    dir: &std::path::Path,
    report: &deepseeknova_agent::diagnose::DiagnoseReport,
) {
    if report.failures.is_empty() {
        return;
    }
    let retry_rounds = report.failures.len() as u32;
    if let Err(e) = deepseeknova_metrics::update_scorecard_task_rate(
        dir,
        &report.session_id,
        false,
        retry_rounds,
    ) {
        tracing::warn!("scorecard task_rate update failed: {e}");
    }
}

/// [`attach_diagnose_hook`] 的协议增强扩展：`[protocol] enabled=true` 且
/// `workspace_root` 非空时，除原落盘/留存逻辑外，把本会话
/// [`DiagnoseReport`](deepseeknova_agent::diagnose::DiagnoseReport) 的
/// `failures` 逐条聚类进
/// [`FailurePatternStore`](deepseeknova_security::failure_pattern::FailurePatternStore)
/// （`<workspace_root>/.deepseeknova/security/failure-patterns.json`，协议增强
/// 设计 §6）并 save。字段映射：`FailureDetail.phase → phase`、
/// `.tool → tool`、`.error → error`、`.root_cause.or(fix_plan) → lesson`。
/// 注入内容先脱敏（spec §6.2）：error/lesson 过
/// [`redact_secrets`] 再 ingest，防止密钥原文进模式库并被下会话回灌进
/// system prompt（接线侧最后防线；security 侧 ingest 入口另有双保险）。
/// 诊断钩子天然只在非 success 结束时触发，满足「仅失败会话 ingest」语义；
/// 此外无论 `[protocol]` 开关，回调都会对同会话评分卡做 task_rate 回填
/// （设计 §7.1 末条：按 failures 推导 `first_pass`/`retry_rounds` 覆写并
/// 重写 `dir/<session_id>.scorecard.json`，补 Paused 路径上 metrics hook
/// 先触发时缺失的失败信息；**仅 failures 非空时覆写**，零失败报告保持
/// metrics 侧已填值，见 P-L2；评分卡不存在时静默跳过）；
/// 所有 IO 失败仅 warn，不阻断 run。`None` 配置或 disabled 时与
/// [`attach_diagnose_hook`] 完全一致。
pub fn attach_diagnose_hook_with_ingest(
    agent: deepseeknova_agent::Agent,
    dir: PathBuf,
    config: Option<&Config>,
    workspace_root: &std::path::Path,
) -> deepseeknova_agent::Agent {
    let diagnose_dir = dir.join("diagnose");
    // 协议增强：聚类仅在 `[protocol] enabled` 且提供 workspace 时启用。
    let ingest_on = config
        .map(|c| c.protocol.enabled && !workspace_root.as_os_str().is_empty())
        .unwrap_or(false);
    let patterns_path = workspace_root
        .join(".deepseeknova")
        .join("security")
        .join("failure-patterns.json");
    let hook: deepseeknova_agent::diagnose::DiagnoseHook = Arc::new(move |report| {
        // DiagnoseReport::write_to 负责建目录 + 写 `<session_id>.json`；
        // 失败仅 warn（与 attach_metrics_hook 落盘同模式，不阻断 run）。
        if let Err(e) = report.write_to(&diagnose_dir) {
            tracing::warn!("diagnose report write failed: {e}");
            return;
        }
        enforce_metrics_retention(&diagnose_dir, DIAGNOSE_RETENTION_MAX);

        // task_rate 回填（设计 §7.1 末条）：Paused/unverified 路径上 metrics
        // hook 先于本回调触发（失败详情尚不可知，评分卡已按保守默认 false/0
        // 落盘），此处按本会话 failures 覆写并重写同会话评分卡
        // （`<dir>/<session_id>.scorecard.json`）。零失败（Cancelled/unverified
        // 无工具失败详情）**不覆写**，保持 metrics hook 已填的值，避免零失败
        // 会话被误标 first_pass=true（P-L2）。评分卡缺失/不可解析时静默跳过
        // （metrics 未启用），不 panic、不阻断。
        backfill_scorecard_task_rate(&dir, &report);

        // 协议增强：失败模式聚类（仅 protocol.enabled 时动作）。
        if ingest_on {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            match deepseeknova_security::failure_pattern::FailurePatternStore::load(&patterns_path)
            {
                Ok(mut store) => {
                    ingest_failure_patterns(&mut store, &report.failures, now_ms);
                    if let Err(e) = store.save() {
                        tracing::warn!("failure pattern store save failed: {e}");
                    }
                }
                Err(e) => tracing::warn!("failure pattern store load failed: {e}"),
            }
        }
    });
    agent.with_diagnose_hook(hook)
}

/// 失败模式聚类写入（协议增强 §6）：把 `failures` 逐条 ingest 进 store。
/// 注入内容脱敏（spec §6.2）：每条 `FailureDetail` 的 `error` 与 `lesson`
/// （`root_cause.or(fix_plan)`）先过 [`redact_secrets`] 再 ingest，防止密钥/
/// 凭据原文进 failure-patterns.json 并被下会话回灌进 system prompt（接线侧
/// 最后防线；security 侧 ingest 入口另有双保险）。
fn ingest_failure_patterns(
    store: &mut deepseeknova_security::failure_pattern::FailurePatternStore,
    failures: &[deepseeknova_agent::diagnose::FailureDetail],
    now_ms: u64,
) {
    for f in failures {
        let lesson = f
            .root_cause
            .as_deref()
            .or(f.fix_plan.as_deref())
            .map(str::to_string);
        let error = redact_secrets(&f.error);
        let lesson = lesson.as_deref().map(redact_secrets);
        store.ingest(
            &f.phase,
            f.tool.as_deref(),
            &error,
            lesson.as_deref(),
            now_ms,
        );
    }
}

/// 失败模式回灌（协议增强设计 §6.2）：`[protocol] enabled=true` 时，会话
/// 启动前（run 开始前）从 `<workspace_root>/.deepseeknova/security/
/// failure-patterns.json` 加载历史失败模式库，`suggest(3)` 取 top-3 后追加
/// `## 本会话已知失败模式（自动注入）` 块到首轮 system prompt（复用
/// `Agent::with_appended_system_prompt` 先例，见 graph 检索提示注入）。
///
/// 无模式 / store 缺失 / IO 失败时零注入（仅 warn），`enabled=false` 时
/// 原样返回，Agent 行为零变化。本函数只做回灌，不涉及门控/对抗审查
/// （见 [`attach_protocol_gates`]）。
pub fn attach_failure_pattern_injection(
    agent: deepseeknova_agent::Agent,
    config: &Config,
    workspace_root: &std::path::Path,
) -> deepseeknova_agent::Agent {
    if !config.protocol.enabled {
        return agent;
    }
    let path = workspace_root
        .join(".deepseeknova")
        .join("security")
        .join("failure-patterns.json");
    let suggestions = match deepseeknova_security::failure_pattern::FailurePatternStore::load(&path)
    {
        Ok(store) => store.suggest(3),
        Err(e) => {
            tracing::warn!("failure pattern store load failed: {e}");
            Vec::new()
        }
    };
    if suggestions.is_empty() {
        return agent;
    }
    let mut block = String::from("\n\n## 本会话已知失败模式（自动注入）\n");
    for s in &suggestions {
        block.push_str(&format!("- {s}\n"));
    }
    agent.with_appended_system_prompt(block)
}

/// 协议门控装配（协议增强设计 §3.3/§3.4）：`[protocol] enabled=true` 时，
/// 解析 `config.protocol.gates`（`HashMap<String, String>`，值 `hard|soft|off`，
/// 非法值 warn 跳过该项；缺省门名用 `builtin_phase_gates` 内置默认力度表）
/// → `agent::phase_runner::builtin_phase_gates(&levels)` →
/// `Agent::with_protocol_gates`。`enabled=false`（默认）原样返回，Agent
/// 行为零变化（零成本路径，见 phase_runner 文档）。
///
/// 对抗审查（设计 §4.2）：`config.protocol.adversarial_review=true` 时调用
/// `Agent::with_adversarial_review(true)`——开关独立于 `enabled`，E 侧
/// 全包 spawn/写报告，runtime 只传开关。
///
/// `workspace_root` 暂未使用（预留：未来门配置可能含工作区相对路径），
/// 保持签名与装配链其他函数一致。
pub fn attach_protocol_gates(
    agent: deepseeknova_agent::Agent,
    config: &Config,
    _workspace_root: &std::path::Path,
) -> deepseeknova_agent::Agent {
    let mut agent = agent;
    if config.protocol.enabled {
        use deepseeknova_agent::phase_runner::GateLevel;
        use std::str::FromStr;

        let mut levels: std::collections::HashMap<String, GateLevel> =
            std::collections::HashMap::new();
        for (name, raw) in &config.protocol.gates {
            match GateLevel::from_str(raw) {
                Ok(level) => {
                    levels.insert(name.clone(), level);
                }
                Err(e) => {
                    tracing::warn!("protocol gate '{name}' skipped: {e} (config value '{raw}')");
                }
            }
        }
        let gates = deepseeknova_agent::phase_runner::builtin_phase_gates(&levels);
        agent = agent.with_protocol_gates(gates);
    }
    if config.protocol.adversarial_review {
        agent = agent.with_adversarial_review(true);
    }
    agent
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
    use deepseeknova_agent::task_spec::InputValues;

    let mut presets = deepseeknova_agent::builtin_presets();
    for ov in &config.delegate.agents {
        if let Some(p) = presets.iter_mut().find(|p| p.name == ov.name) {
            if let Some(sp) = &ov.system_prompt {
                p.system_prompt = sp.clone();
            }
            if let Some(tools) = &ov.tools {
                p.spec.tools = tools.clone();
            }
            if let Some(ms) = ov.max_steps {
                p.spec.max_steps = ms;
            }
            if let Some(inputs) = &ov.inputs {
                p.config_inputs = InputValues::from(
                    inputs
                        .iter()
                        .map(|i| (i.name.clone(), i.value.clone()))
                        .collect::<std::collections::HashMap<_, _>>(),
                );
            }
        } else {
            let mut preset = deepseeknova_agent::DelegatePreset::simple(
                ov.name.clone(),
                ov.system_prompt.clone().unwrap_or_default(),
                ov.tools.clone().unwrap_or_default(),
                ov.max_steps.unwrap_or(10),
            );
            if let Some(inputs) = &ov.inputs {
                preset.config_inputs = InputValues::from(
                    inputs
                        .iter()
                        .map(|i| (i.name.clone(), i.value.clone()))
                        .collect::<std::collections::HashMap<_, _>>(),
                );
            }
            presets.push(preset);
        }
    }
    presets
}

/// 构建委派引擎：合并内置预设与配置覆盖，为每个预设造一个受限工具集的子 Agent
/// （共享主 agent 的 graph/memory 句柄与安全策略）。禁递归：剔除任何 "delegate" 工具。
/// `attribution` 提供子代理失败归因重试（None = 旧行为，失败直接上抛）。
#[allow(clippy::too_many_arguments)]
fn build_delegate_engine(
    config: &Config,
    provider: Arc<dyn deepseeknova_provider::Provider>,
    workspace_root: &std::path::Path,
    security: &SecurityContext,
    gate: Option<Arc<PermissionGate>>,
    graph_ext: Option<deepseeknova_tools::GraphHandle>,
    memory_ext: Option<deepseeknova_tools::MemoryHandle>,
    attribution: Option<Arc<deepseeknova_agent::attribution::AttributionSettings>>,
) -> Arc<deepseeknova_agent::DelegateEngine> {
    use deepseeknova_core::Tool;

    // 子代理工具源（沿用主 agent 的沙箱选择）。
    // 注：子代理刻意不接收 MCP/extra_tools——它们只从内置工具集派生受限子集，
    // 因此 MCP 工具天然只暴露给主 agent（等价于向 build_agent 传 vec![]）。
    let base: Vec<Arc<dyn Tool>> = if config.sandbox.enabled {
        let sandbox: Arc<dyn deepseeknova_sandbox::Sandbox> =
            Arc::from(deepseeknova_sandbox::platform_sandbox_with(
                &sandbox_writable_paths(config, workspace_root),
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
                n != "delegate" && p.spec.tools.iter().any(|allow| allow == &n)
            })
            .cloned()
            .collect();
        let mut sub = deepseeknova_agent::Agent::new(Arc::clone(&provider), p.spec.max_steps)
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
            // deny 冻结（prompt 层）：父级 deny 规则注入子代理 system prompt，
            // 让子代理模型在发起调用前即知晓禁止边界。执行层仍由共享 gate 强制。
            if let Some(frozen) = render_frozen_denies(gate.deny_rules()) {
                sub = sub.with_system_prompt(format!(
                    "{}\n\n## 禁止操作（父级冻结，不可执行）\n{frozen}",
                    p.system_prompt
                ));
            }
        }
        agents.insert(p.name.clone(), Arc::new(sub));
    }

    let mut engine = deepseeknova_agent::DelegateEngine::new(
        agents,
        config.delegate.max_concurrent,
        config.delegate.output_cap_tokens,
    );
    for p in &presets {
        engine.register_spec(p.name.clone(), p.spec.clone(), p.config_inputs.clone());
    }
    if let Some(a) = attribution {
        engine = engine.with_attribution((*a).clone());
    }
    Arc::new(engine)
}

/// 把 deny 规则渲染为冻结清单文本（供子代理 system prompt 注入）。
/// 空规则返回 `None`（不产生追加）。
fn render_frozen_denies(rules: &[deepseeknova_permission::Rule]) -> Option<String> {
    if rules.is_empty() {
        return None;
    }
    let lines: Vec<String> = rules
        .iter()
        .map(|r| match &r.subject {
            Some(s) => format!("- {} {s}", r.tool),
            None => format!("- {}", r.tool),
        })
        .collect();
    Some(lines.join("\n"))
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
    frozen_denies: &[deepseeknova_permission::Rule],
    permission_gate: Option<Arc<deepseeknova_permission::PermissionGate>>,
    security: Option<deepseeknova_security::context::SecurityContext>,
    workspace_root: &std::path::Path,
) -> deepseeknova_agent::SubAgentRunner {
    use deepseeknova_core::Tool;

    // deny 冻结（prompt 层）：渲染一次，注入每个子代理 system prompt
    let frozen_lines: Vec<String> = render_frozen_denies(frozen_denies)
        .map(|s| s.lines().map(|l| l.to_string()).collect())
        .unwrap_or_default();

    // 子代理工具源（与委派引擎同款：沿用沙箱选择，不接 MCP 工具）。
    let base: Vec<Arc<dyn Tool>> = if config.sandbox.enabled {
        let sandbox: Arc<dyn deepseeknova_sandbox::Sandbox> =
            Arc::from(deepseeknova_sandbox::platform_sandbox_tiered(
                deepseeknova_sandbox::SandboxTier::WorkspaceWrite,
                &sandbox_writable_paths(config, workspace_root),
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
                n != "delegate" && p.spec.tools.iter().any(|allow| allow == &n)
            })
            .cloned()
            .collect();
        runner.register(
            deepseeknova_agent::SubAgentConfig::new(p.name.clone(), p.system_prompt.clone())
                // P2 修复：把预设的任务书（含 inputs 声明与 task 模板）接入
                // SubAgentRunner 渲染路径。当前 config 只能产出 simple spec
                // （无 inputs 声明 → render 返回空 → 行为与接线前完全一致）；
                // 未来 spec 声明源就位后此路径立即生效。with_tools/with_max_steps
                // 在其后调用，保持 spec.tools/spec.max_steps 与执行参数同步。
                .with_task_spec(p.spec.clone())
                .with_frozen_denies(frozen_lines.clone())
                .with_tools(sub_tools)
                .with_max_steps(p.spec.max_steps)
                .with_config_inputs(p.config_inputs.clone()),
        );
    }
    if let Some(threshold) = derive_compaction_threshold(config) {
        runner = runner.with_compaction_threshold(threshold);
    }
    if let Some(compact) = compact_provider {
        runner = runner.with_compact_provider(compact);
    }
    if let Some(gate) = permission_gate {
        // 执行层权限强制：子代理工具调用在 execute 前经 gate 检查
        //（与 Agent 型 delegate engine 的 with_permission_gate 对齐）。
        runner = runner.with_permission_gate(gate);
    }
    if let Some(sec) = security {
        // 执行上下文装配：shell/fs/web 工具强依赖 SecurityContext
        //（缺失时 enforce_capability 直接报错，子代理工具面不可用）。
        runner = runner.with_security(sec);
    }
    runner = runner.with_workspace_root(workspace_root);
    runner
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_config::Config;
    use deepseeknova_context::ContextEngine;
    use deepseeknova_core::memory::skill::{SkillExtractionConfig, SkillManager, SkillState};
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
                content: "ok".to_string(),
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

    #[test]
    fn sandbox_writable_paths_includes_workspace_once() {
        let mut config = Config::default();
        config.sandbox.writable_paths = vec!["/tmp/work".into()];
        let root = std::path::Path::new("/ws");
        let paths = sandbox_writable_paths(&config, root);
        assert_eq!(paths, vec!["/tmp/work".to_string(), "/ws".to_string()]);

        // 已显式包含工作区根时不重复添加
        config.sandbox.writable_paths = vec!["/ws".into(), "/tmp/work".into()];
        let paths = sandbox_writable_paths(&config, root);
        assert_eq!(paths, vec!["/ws".to_string(), "/tmp/work".to_string()]);
    }

    /// 空内容 provider：agent 每步无输出 → MaxSteps → Paused（构造失败 run
    /// 用；默认 `stream` 只透出 TextDelta+Done，Message 带 tool_calls 也不会
    /// 触发工具执行，故空内容即可稳定命中 MaxSteps 路径）。
    struct EmptyProvider;

    #[async_trait::async_trait]
    impl deepseeknova_provider::Provider for EmptyProvider {
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
        let runner = build_sub_agent_runner(
            &config,
            task,
            Some(compact.clone()),
            &[],
            None,
            None,
            &std::env::temp_dir(),
        );

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
    fn compaction_threshold_derives_from_budget() {
        let mut c = Config::default(); // budget 默认启用、max_total=128000
        assert_eq!(derive_compaction_threshold(&c), Some(64_000));
        c.agent.compaction_threshold_tokens = Some(32_000);
        assert_eq!(derive_compaction_threshold(&c), Some(32_000)); // 显式优先
        c.agent.compaction_threshold_tokens = None;
        c.budget.enabled = false;
        assert_eq!(derive_compaction_threshold(&c), None); // budget 关 → None
    }

    /// 参数化任务书在 SubAgentRunner 路径的渲染生效证明：spec 含 inputs 声明
    /// 与 `${{ inputs.x }}` 占位符时，prompt 协议 `input:` 行传入的值必须渲染
    /// 进子代理消息（task 追加 User、RULES 追加 System）。无 spec/无 input 行
    /// 时渲染为空 = 行为不变（既有 sub_agent_runner_registers_presets 测试守护）。
    #[tokio::test]
    async fn sub_agent_task_spec_inputs_render_into_prompt() {
        use deepseeknova_agent::task_spec::{InputSpec, InputType, TaskSpec};
        use deepseeknova_core::Runner;
        use futures::StreamExt;
        use std::sync::Mutex;

        // 捕获 provider 收到的全部消息文本。不覆写 stream：默认 stream 回退
        // 到 generate，在此处截获 messages 即可覆盖子代理每次调用的输入。
        struct CapturingProvider {
            seen: Arc<Mutex<Vec<String>>>,
        }
        #[async_trait::async_trait]
        impl deepseeknova_provider::Provider for CapturingProvider {
            async fn generate(
                &self,
                v: deepseeknova_provider::ValidatedRequest<'_>,
            ) -> anyhow::Result<deepseeknova_core::Message> {
                let mut texts: Vec<String> = v.messages.iter().map(|m| m.content.clone()).collect();
                self.seen.lock().unwrap().append(&mut texts);
                Ok(deepseeknova_core::Message {
                    role: deepseeknova_core::Role::Assistant,
                    content: "ok".to_string(),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                })
            }
        }

        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let provider: Arc<dyn deepseeknova_provider::Provider> =
            Arc::new(CapturingProvider { seen: seen.clone() });
        let spec = TaskSpec {
            name: "reviewer".into(),
            task: "Review ${{ inputs.path }} carefully".into(),
            rules: vec!["Do not modify files".into()],
            inputs: vec![InputSpec {
                name: "path".into(),
                ty: InputType::String,
                required: true,
                default: None,
            }],
            tools: Vec::new(),
            max_steps: 2,
        };
        let mut runner = deepseeknova_agent::SubAgentRunner::new(provider);
        runner.register(
            deepseeknova_agent::SubAgentConfig::new("reviewer", "you are a reviewer")
                .with_task_spec(spec)
                .with_max_steps(2),
        );
        let runner = runner.with_default("reviewer");

        let mut stream = runner
            .run_stream(deepseeknova_core::RunInput {
                prompt: "sub_agent:reviewer\ninput:path=src/lib.rs\ngoal:review the change".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        let texts = seen.lock().unwrap();
        let all: String = texts.join("\n");
        assert!(
            all.contains("Review src/lib.rs carefully"),
            "占位符必须被 input: 行渲染替换，实得: {all}"
        );
        assert!(
            all.contains("## RULES\n- Do not modify files"),
            "RULES 必须渲染进消息，实得: {all}"
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
                inputs: None,
            });
        let task = std::sync::Arc::new(stub_provider());
        let _runner =
            build_sub_agent_runner(&config, task, None, &[], None, None, &std::env::temp_dir());
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
        assert!(names.iter().any(|n| n == "trace_code"));
        assert!(names.iter().any(|n| n == "impact_code"));
        assert!(names.iter().any(|n| n == "explore_code"));
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
        assert!(!agent.tool_names().iter().any(|n| n == "trace_code"));
        assert!(!agent.tool_names().iter().any(|n| n == "impact_code"));
        assert!(!agent.tool_names().iter().any(|n| n == "explore_code"));
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
        assert!(
            names.iter().any(|n| n == "context7_docs"),
            "文档检索工具应常驻注册"
        );
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
    async fn build_agent_wires_llm_distill_and_runs_without_panic() {
        use futures::StreamExt;
        let mut config = Config::default();
        config.memory.enabled = true;
        config.memory.llm_distill = true;
        config.graph.enabled = false;
        config.verify.enabled = false;
        config.review.enabled = false;
        let root = std::env::temp_dir().join(format!("dnv-llm-distill-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, root.clone(), provider, 5, None, vec![]).unwrap();

        // 跑一轮：stub 返回空文本，事件流正常结束（Done 或跑满步数 Paused）；
        // LLM 蒸馏不可解析 → 静默跳过，不 panic。
        let mut stream = agent
            .run_stream(RunInput {
                prompt: "hi".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        // 记忆引擎仍可打开并列出（蒸馏失败不影响记忆库可用性）。
        let engine = deepseeknova_core::memory::engine::MemoryEngine::open(
            root.join(".deepseeknova/memory.db"),
            true,
        )
        .unwrap();
        let _ = engine
            .list(deepseeknova_core::memory::store::MemoryCategory::Skill)
            .unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 递归列出 skills 目录（诊断辅助）。
    fn walk_skills_tree(dir: &std::path::Path) -> std::io::Result<()> {
        if !dir.exists() {
            eprintln!("[diag]   (skills dir does not exist)");
            return Ok(());
        }
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                eprintln!("[diag]   dir: {}", path.display());
                let _ = walk_skills_tree(&path);
            } else {
                eprintln!("[diag]   file: {}", path.display());
            }
        }
        Ok(())
    }

    /// 设计 C 集成测试：蒸馏产出 → auto/ 落盘（frontmatter 含 source: distill）
    /// → reload 后状态保持 → recall 匹配注入。
    #[tokio::test]
    async fn build_agent_distill_writes_auto_skill_and_recall_injects() {
        use futures::StreamExt;
        let mut config = Config::default();
        config.memory.enabled = true;
        config.memory.llm_distill = true;
        // stub provider 不产生工具调用 → 蒸馏门槛调低，保证 skill 分支落盘
        config.memory.min_tool_calls = 0;
        config.memory.min_steps = 0;
        config.graph.enabled = false;
        config.verify.enabled = false;
        config.review.enabled = false;
        let root = std::env::temp_dir().join(format!("dnv-skill-hot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        // Provider 返回可解析的蒸馏 JSON（主循环视作普通 assistant 文本，
        // 回合结束蒸馏可解析 → 落盘 skill）。
        struct SkillProvider;
        #[async_trait::async_trait]
        impl deepseeknova_provider::Provider for SkillProvider {
            async fn generate(
                &self,
                _v: deepseeknova_provider::ValidatedRequest<'_>,
            ) -> anyhow::Result<deepseeknova_core::Message> {
                Ok(deepseeknova_core::Message {
                    role: deepseeknova_core::Role::Assistant,
                    content: r#"{"kind":"skill","title":"Fix Auth Flow","body":"Validate tokens first","tags":["auth"]}"#
                        .into(),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                })
            }
        }
        let provider: Arc<dyn deepseeknova_provider::Provider> = Arc::new(SkillProvider);
        let agent = build_agent(&config, root.clone(), provider, 5, None, vec![]).unwrap();
        let mut stream = agent
            .run_stream(RunInput {
                prompt: "hi".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        // 异步蒸馏 spawn 无法 join → 轮询等待落盘文件出现。
        let auto_path = root.join(".deepseeknova/skills/auto/fix-auth-flow.md");
        // 诊断：确认 run 后蒸馏目录与记忆库状态。
        let skills_dir = root.join(".deepseeknova/skills");
        let db_path = root.join(".deepseeknova/memory.db");
        eprintln!(
            "[diag] after run: skills_dir={:?} exists={} db={:?} exists={}",
            skills_dir,
            skills_dir.exists(),
            db_path,
            db_path.exists()
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !auto_path.exists() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        if !auto_path.exists() {
            eprintln!("[diag] auto skill NOT written after 5s; skills tree:");
            let _ = walk_skills_tree(&skills_dir);
        }
        assert!(auto_path.exists(), "蒸馏 skill 应落盘 auto/ 子目录");
        let content = std::fs::read_to_string(&auto_path).unwrap();
        assert!(
            content.contains("source: distill"),
            "frontmatter 必须含 source: distill"
        );
        assert!(content.contains("state: draft"), "初始态必须是 draft");

        // reload 后状态保持 + recall 注入前置：全新实例重开同一目录
        let m = SkillManager::new(SkillExtractionConfig {
            skill_dir: root.join(".deepseeknova/skills"),
            ..Default::default()
        });
        assert_eq!(m.skill_state("fix-auth-flow"), Some(SkillState::Draft));
        let matched = m.find_matching_skills("auth");
        assert!(
            !matched.is_empty(),
            "reload 后 recall 应能匹配到该 distill skill"
        );
        assert!(
            matched
                .iter()
                .any(|s| s.frontmatter.name == "fix-auth-flow"),
            "匹配结果应含 fix-auth-flow"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn build_agent_wires_reflection_and_runs_without_panic() {
        use futures::StreamExt;
        let mut config = Config::default(); // reflect_on_failure 默认 true
        config.graph.enabled = false;
        config.verify.enabled = false;
        config.review.enabled = false;
        let root = std::env::temp_dir().join(format!("dnv-reflect-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, root.clone(), provider, 5, None, vec![]).unwrap();

        // 一轮 run 正常结束（无失败回炉则反思不触发，但装配路径必须不 panic）。
        let mut stream = agent
            .run_stream(RunInput {
                prompt: "hi".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        // 反思教训钩子挂在记忆引擎上，库仍可打开。
        let engine = deepseeknova_core::memory::engine::MemoryEngine::open(
            root.join(".deepseeknova/memory.db"),
            true,
        )
        .unwrap();
        let _ = engine
            .list(deepseeknova_core::memory::store::MemoryCategory::Skill)
            .unwrap();
        let _ = std::fs::remove_dir_all(&root);
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
            None,
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
            None,
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

    fn mid_run_test_workspace(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dnv-midrun-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn mid_run_config_off_leaves_agent_unwired() {
        let mut config = Config::default();
        config.graph.enabled = false;
        config.memory.mid_run_recall = false;
        let workspace = mid_run_test_workspace("off");
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent_with_role_providers(
            &config,
            workspace.clone(),
            provider,
            AgentRoleProviders::default(),
            5,
            None,
            vec![],
            None,
        )
        .unwrap();
        assert!(
            !agent.mid_run_retrieval_enabled(),
            "mid_run_recall=false must not wire mid-run retrieval"
        );
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn mid_run_config_on_wires_retrieval() {
        let mut config = Config::default();
        config.graph.enabled = false;
        config.memory.mid_run_recall = true;
        let workspace = mid_run_test_workspace("on");
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent_with_role_providers(
            &config,
            workspace.clone(),
            provider,
            AgentRoleProviders::default(),
            5,
            None,
            vec![],
            None,
        )
        .unwrap();
        assert!(
            agent.mid_run_retrieval_enabled(),
            "mid_run_recall=true must wire mid-run retrieval"
        );
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn repo_map_seeds_extracts_identifiers_and_skips_stopwords() {
        let seeds =
            repo_map_seeds("please fix CheckpointManager and add tests for repo_map wiring");
        assert!(seeds.iter().any(|s| s == "CheckpointManager"));
        assert!(seeds.iter().any(|s| s == "repo_map"));
        assert!(
            !seeds
                .iter()
                .any(|s| matches!(s.as_str(), "please" | "and" | "fix" | "add" | "for")),
            "stopwords must be excluded, got {seeds:?}"
        );

        let many =
            repo_map_seeds("alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu");
        assert!(many.len() <= 8, "seed cap must hold, got {many:?}");
        let deduped = repo_map_seeds("token token token again again");
        assert!(deduped.len() <= 2, "seeds must dedupe, got {deduped:?}");
    }

    #[test]
    fn graph_retrieval_hint_stays_english_and_graph_first() {
        for tool in ["search_code", "traverse_graph", "retrieve_entity"] {
            assert!(GRAPH_RETRIEVAL_HINT.contains(tool), "hint missing {tool}");
        }
        assert!(
            !GRAPH_RETRIEVAL_HINT.contains("检索"),
            "hint must be English, not Chinese"
        );
    }

    #[tokio::test]
    async fn metrics_enabled_writes_one_report_per_run() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;

        let dir = std::env::temp_dir().join(format!("dsn-metrics-on-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut config = Config::default();
        config.metrics.enabled = true;
        config.graph.enabled = false;
        config.memory.enabled = false;
        config.delegate.enabled = false;
        let ledger = Arc::new(deepseeknova_provider::cost::CostLedger::new());
        let agent = attach_metrics_hook(
            deepseeknova_agent::Agent::new(Arc::new(stub_provider()), 3),
            &config,
            MetricsSink {
                ledger,
                prices: Default::default(),
                dir: dir.clone(),
            },
        );
        let mut stream = agent
            .run_stream(deepseeknova_core::RunInput {
                prompt: "hi".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        while let Some(ev) = stream.next().await {
            ev.unwrap();
        }
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2, "expected report + scorecard files");
        let report_name = names
            .iter()
            .find(|n| n.ends_with(".json") && !n.ends_with(".scorecard.json"))
            .expect("report file missing");
        let card_name = names
            .iter()
            .find(|n| n.ends_with(".scorecard.json"))
            .expect("scorecard file missing");
        let report: deepseeknova_metrics::SessionReport =
            serde_json::from_str(&std::fs::read_to_string(dir.join(report_name)).unwrap()).unwrap();
        assert_eq!(
            report.stats.outcome,
            Some(deepseeknova_metrics::RunOutcome::Completed)
        );
        assert_eq!(report.stats.steps, 1);
        // 评分卡：与报告同会话 id；无 finding/无失败/空审查 → governance 1.0。
        let card: deepseeknova_metrics::Scorecard =
            serde_json::from_str(&std::fs::read_to_string(dir.join(card_name)).unwrap()).unwrap();
        assert_eq!(card.session_id, report.session_id);
        assert_eq!(card.dimensions.governance, 1.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn metrics_disabled_writes_nothing() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;

        let dir = std::env::temp_dir().join(format!("dsn-metrics-off-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut config = Config::default();
        config.metrics.enabled = false;
        config.graph.enabled = false;
        config.memory.enabled = false;
        config.delegate.enabled = false;
        let ledger = Arc::new(deepseeknova_provider::cost::CostLedger::new());
        let agent = attach_metrics_hook(
            deepseeknova_agent::Agent::new(Arc::new(stub_provider()), 3),
            &config,
            MetricsSink {
                ledger,
                prices: Default::default(),
                dir: dir.clone(),
            },
        );
        let mut stream = agent
            .run_stream(deepseeknova_core::RunInput {
                prompt: "hi".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        while let Some(ev) = stream.next().await {
            ev.unwrap();
        }
        assert!(!dir.exists(), "metrics disabled must not create output dir");
    }

    /// 任务质量闭环 B：attach_diagnose_hook 仅失败 run 在 `<dir>/diagnose/`
    /// 落盘 `<session_id>.json`；成功 run 不产生新文件。
    #[tokio::test]
    async fn diagnose_hook_writes_report_for_failed_run_only() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;

        let dir = std::env::temp_dir().join(format!("dsn-diag-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        // 失败 run：空内容 → MaxSteps → Paused → 报告落盘。
        let agent = attach_diagnose_hook(
            deepseeknova_agent::Agent::new(Arc::new(EmptyProvider), 2),
            dir.clone(),
        );
        let mut stream = agent
            .run_stream(deepseeknova_core::RunInput {
                prompt: "hi".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        while let Some(ev) = stream.next().await {
            ev.unwrap();
        }
        let diag_dir = dir.join("diagnose");
        let mut files: Vec<String> = std::fs::read_dir(&diag_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(files.len(), 1, "expected exactly one diagnose report");
        let report: deepseeknova_agent::diagnose::DiagnoseReport =
            serde_json::from_str(&std::fs::read_to_string(diag_dir.join(&files[0])).unwrap())
                .unwrap();
        assert_eq!(report.outcome, "paused");
        assert!(!report.phases.is_empty(), "phases must be recorded");
        assert!(!report.failures.is_empty(), "failures must be non-empty");

        // 成功 run：不新增报告文件。
        let agent = attach_diagnose_hook(
            deepseeknova_agent::Agent::new(Arc::new(stub_provider()), 3),
            dir.clone(),
        );
        let mut stream = agent
            .run_stream(deepseeknova_core::RunInput {
                prompt: "hi".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        while let Some(ev) = stream.next().await {
            ev.unwrap();
        }
        files = std::fs::read_dir(&diag_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(files.len(), 1, "success must not add a diagnose report");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P3：留存助手单测——报告数超上限时删最旧（按 mtime），新文件保留。
    /// 文件名故意与创建顺序相反，若实现误按文件名排序本测试会失败。
    #[test]
    fn metrics_retention_helper_removes_oldest_beyond_max() {
        let dir = std::env::temp_dir().join(format!("dsn-metrics-helper-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 评分卡最先创建（mtime 最旧）：即便按 mtime 是"最旧候选"，也因
        // `.scorecard.json` 排除规则永不参与裁剪。大写扩展名变体同规则
        // （F12：大小写不敏感）——同样最旧、同样永不裁剪。
        std::fs::write(dir.join("oldest.scorecard.json"), "{}").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(25));
        std::fs::write(dir.join("OLD.SCORECARD.JSON"), "{}").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(25));

        // 创建顺序 old → new，但名字排序相反（z 最旧、a 最新）。
        for (name, i) in [
            ("z.json", 0usize),
            ("m.json", 1),
            ("a.json", 2),
            ("k.json", 3),
        ] {
            std::fs::write(dir.join(name), "{}").unwrap();
            if i < 3 {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }
        // 非 json 文件不受影响。
        std::fs::write(dir.join("README.txt"), "x").unwrap();

        enforce_metrics_retention(&dir, 2);

        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "OLD.SCORECARD.JSON",
                "README.txt",
                "a.json",
                "k.json",
                "oldest.scorecard.json",
            ],
            "应删最旧两个（z/m），保留最新两个 json + 非 json 文件 + 大小写两种 scorecard（永不裁剪）"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P3 集成：attach_metrics_hook 落盘后按 `[metrics] max_reports` 清理。
    /// 预置 max 个旧报告（mtime 递增），本轮 run 写第 max+1 个 → 最旧的被删。
    #[tokio::test]
    async fn metrics_retention_trims_oldest_reports_beyond_max() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;

        let dir = std::env::temp_dir().join(format!("dsn-metrics-ret-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let max = 3usize;
        for i in 0..max {
            // 名字与创建顺序相反（z 最旧、a 最新），防止误按名字排序的假通过。
            let name = ["z", "m", "a"][i];
            std::fs::write(dir.join(format!("{name}.json")), "{}").unwrap();
            if i + 1 < max {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }

        let mut config = Config::default();
        config.metrics.enabled = true;
        config.metrics.max_reports = max;
        config.graph.enabled = false;
        config.memory.enabled = false;
        config.delegate.enabled = false;
        let ledger = Arc::new(deepseeknova_provider::cost::CostLedger::new());
        let agent = attach_metrics_hook(
            deepseeknova_agent::Agent::new(Arc::new(stub_provider()), 3),
            &config,
            MetricsSink {
                ledger,
                prices: Default::default(),
                dir: dir.clone(),
            },
        );
        let mut stream = agent
            .run_stream(deepseeknova_core::RunInput {
                prompt: "hi".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        while let Some(ev) = stream.next().await {
            ev.unwrap();
        }

        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        // 本轮 run 写入 report + scorecard 两份；scorecard 不占留存名额，
        // 参与裁剪的普通报告 = z/m/a + 本轮 report 共 4 份 → max=3 只删最旧
        // 一份（z），保留 m/a + 本轮 report + 本轮 scorecard。
        assert_eq!(
            names.len(),
            max + 1,
            "清理后应保留 max 个普通报告 + 1 个 scorecard（scorecard 永不裁剪）"
        );
        assert!(
            !names.contains(&"z.json".to_string()),
            "最旧报告 z 必须被删"
        );
        assert!(
            names.contains(&"m.json".to_string()),
            "scorecard 不占留存名额，次旧报告 m 必须保留"
        );
        assert!(names.contains(&"a.json".to_string()));
        // 新文件（本轮 run 写入）：report + scorecard 两份。
        let newest: Vec<String> = names
            .iter()
            .filter(|n| !["z.json", "m.json", "a.json"].contains(&n.as_str()))
            .cloned()
            .collect();
        assert_eq!(newest.len(), 2, "应保留本轮 report + scorecard 两份");
        assert!(
            newest.iter().any(|n| n.ends_with(".scorecard.json")),
            "本轮 scorecard 必须存活：{newest:?}"
        );
        let report_name = newest
            .iter()
            .find(|n| n.ends_with(".json") && !n.ends_with(".scorecard.json"))
            .expect("new report missing");
        let report: deepseeknova_metrics::SessionReport =
            serde_json::from_str(&std::fs::read_to_string(dir.join(report_name)).unwrap()).unwrap();
        assert_eq!(
            report.stats.outcome,
            Some(deepseeknova_metrics::RunOutcome::Completed)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_agent_with_attribution_enabled_constructs() {
        // [attribution] enabled=true 时主 agent 与 delegate 引擎都装配归因
        // （字段私有，行为由 agent crate 测试覆盖；此处验证装配路径不 panic）。
        let mut config = Config::default();
        config.graph.enabled = false;
        config.memory.enabled = false;
        config.delegate.enabled = true;
        config.attribution.enabled = true;
        config.attribution.max_retries = 2;
        config.attribution.max_attributions = 5;
        config.attribution.degrade_map =
            std::collections::HashMap::from([("researcher".to_string(), "explorer".to_string())]);
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
        let _ = agent;
    }

    #[test]
    fn build_agent_with_attribution_disabled_matches_legacy() {
        // 默认（enabled=false）：不调用 with_attribution，行为零变化；装配路径不 panic。
        let mut config = Config::default();
        config.graph.enabled = false;
        config.memory.enabled = false;
        config.delegate.enabled = true;
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
        let _ = agent;
    }

    /// 预置一个 distill draft skill（命中 recall 查询 "auth..."）。
    fn seed_skill(skills_dir: &std::path::Path, title: &str, tags: Vec<&str>) {
        let mut sm = SkillManager::new(SkillExtractionConfig {
            skill_dir: skills_dir.to_path_buf(),
            ..Default::default()
        });
        sm.create_distilled_skill(
            title,
            "Validate tokens first",
            tags.into_iter().map(str::to_string).collect(),
            Some("seed"),
        )
        .unwrap();
    }

    /// env 快照守卫：测试结束恢复变量原值（防并行测试互相污染）。
    struct EnvRestore(Vec<(&'static str, Option<String>)>);

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            for (key, value) in &self.0 {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    /// 语义嵌入 fail-open：embedder=remote 但缺 key → 装配不炸，run 照常完成
    /// （try_memory_embedder 返回 None，recall 回落纯 FTS）。
    #[tokio::test]
    async fn remote_embedder_without_key_falls_back_to_fts() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;

        let _env = EnvRestore(vec![
            (
                "DEEPSEEKNOVA_EMBED_API_KEY",
                std::env::var("DEEPSEEKNOVA_EMBED_API_KEY").ok(),
            ),
            ("OPENAI_API_KEY", std::env::var("OPENAI_API_KEY").ok()),
        ]);
        std::env::remove_var("DEEPSEEKNOVA_EMBED_API_KEY");
        std::env::remove_var("OPENAI_API_KEY");

        let root = std::env::temp_dir().join(format!("dnv-embed-failopen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut config = Config::default();
        config.memory.enabled = true;
        config.memory.embedder = "remote".to_string();
        config.memory.embed_model = "text-embedding-3-small".to_string();
        config.graph.enabled = false;
        config.review.enabled = false;
        config.verify.enabled = false;
        config.delegate.enabled = false;
        config.memory.llm_distill = false;
        let provider: Arc<dyn deepseeknova_provider::Provider> = Arc::new(stub_provider());
        let agent = build_agent(&config, root.clone(), provider, 5, None, vec![]).unwrap();
        let mut stream = agent
            .run_stream(deepseeknova_core::RunInput {
                prompt: "auth".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}
        let _ = std::fs::remove_dir_all(&root);
    }

    /// P2 回归：超 budget 场景下只对「实际注入 prompt」的 skill 计 use。
    /// 匹配多项但字符预算只容得下第一项（verified 排 draft 前，顺序确定），
    /// 断言注入项 use_count +1、未注入项 use_count 保持 0（draft 不因
    /// 「匹配即计 use」被污染晋升）。
    #[tokio::test]
    async fn skill_recall_counts_only_injected_skills() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;

        let root = std::env::temp_dir().join(format!("dnv-skill-budget-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let skills_dir = root.join(".deepseeknova/skills");
        {
            let mut sm = SkillManager::new(SkillExtractionConfig {
                skill_dir: skills_dir.clone(),
                ..Default::default()
            });
            // Alpha：先经 3 次 record_use 升为 verified（rank 0，注入时排最前）。
            sm.create_distilled_skill(
                "Auth Alpha",
                "fix auth",
                vec!["auth".to_string()],
                Some("seed"),
            )
            .unwrap();
            for _ in 0..deepseeknova_core::memory::skill::VERIFY_USE_THRESHOLD {
                sm.record_use("auth-alpha", true, Some("seed")).unwrap();
            }
            // Beta：draft（rank 2），描述超长 → 预算不足时排 Alpha 后 break。
            sm.create_distilled_skill(
                "Auth Beta",
                &"very long description ".repeat(6),
                vec!["auth".to_string()],
                Some("seed"),
            )
            .unwrap();
        }

        let mut config = Config::default();
        config.memory.enabled = true;
        // 预算收紧：cap_chars = 10*4 = 40 字符。header（20）+ Alpha 行（27）
        // 累计 47 > 40 → Alpha 注入后 Beta 的 check 必 break → 未注入。
        // （check 语义：注入前比较「已累计 lines.len() > budget」。）
        config.memory.recall_inject_tokens = 10;
        config.memory.recall_top_k = 3;
        config.graph.enabled = false;
        config.review.enabled = false;
        config.verify.enabled = false;
        config.delegate.enabled = false;
        config.memory.llm_distill = false;
        let provider: Arc<dyn deepseeknova_provider::Provider> = Arc::new(stub_provider());
        let agent = build_agent(&config, root.clone(), provider, 5, None, vec![]).unwrap();
        let mut stream = agent
            .run_stream(deepseeknova_core::RunInput {
                prompt: "auth".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        let m = SkillManager::new(SkillExtractionConfig {
            skill_dir: skills_dir.clone(),
            ..Default::default()
        });
        let find = |name: &str| -> usize {
            m.list_skills()
                .iter()
                .find(|s| s.frontmatter.name == name)
                .map(|s| s.frontmatter.use_count)
                .unwrap_or(0) as usize
        };
        assert_eq!(
            find("auth-alpha"),
            deepseeknova_core::memory::skill::VERIFY_USE_THRESHOLD as usize + 1,
            "注入项（Alpha）应计 1 次 use（3 次预置 + 1 次注入）"
        );
        assert_eq!(find("auth-beta"), 0, "未注入项（Beta）use_count 不得增长");
        // Beta 未达阈值 → 仍为 draft（未被污染晋升）。
        assert_eq!(m.skill_state("auth-beta"), Some(SkillState::Draft));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 设计 C 三态闭环集成测试：recall 命中 → record_use → 跨 build 会话推进
    /// → draft → verified → active；会话边界（run 结束）prune 超额 draft。
    #[tokio::test]
    async fn skill_use_loop_closes_via_recall_record_use_and_prune() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;

        let root = std::env::temp_dir().join(format!("dnv-skill-loop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let skills_dir = root.join(".deepseeknova/skills");
        seed_skill(&skills_dir, "Fix Auth Flow", vec!["auth"]);

        let mut config = Config::default();
        config.memory.enabled = true;
        config.graph.enabled = false;
        config.review.enabled = false;
        config.verify.enabled = false;
        config.delegate.enabled = false;
        config.memory.llm_distill = false; // 避免异步蒸馏干扰，聚焦 recall 闭环
        let provider: Arc<dyn deepseeknova_provider::Provider> = Arc::new(stub_provider());

        // 三次独立 build_agent = 三个会话边界；每次 recall 命中即 record_use。
        // 注意 find_matching_skills 的 strong 匹配是「skill name/tag 包含 query」，
        // 故 query 用短词 "auth"（tag "auth" 命中）。
        for _ in 0..3 {
            let agent =
                build_agent(&config, root.clone(), provider.clone(), 5, None, vec![]).unwrap();
            let mut stream = agent
                .run_stream(deepseeknova_core::RunInput {
                    prompt: "auth".into(),
                    images: vec![],
                    model_override: None,
                })
                .await
                .unwrap();
            while stream.next().await.is_some() {}
        }

        // use_count=3 → verified；sessions_seen=3（每 build 新会话 id）→ active
        let m = SkillManager::new(SkillExtractionConfig {
            skill_dir: skills_dir.clone(),
            ..Default::default()
        });
        assert_eq!(
            m.skill_state("fix-auth-flow"),
            Some(SkillState::Active),
            "三态推进必须到达 active"
        );
        let content = std::fs::read_to_string(skills_dir.join("auto/fix-auth-flow.md")).unwrap();
        assert!(content.contains("use_count: 3"), "use_count 必须落盘为 3");
        assert!(content.contains("state: active"), "state 必须落盘为 active");

        // 清理阶段：再灌 22 个不匹配的 draft，跑一轮 → 会话边界 prune 到 20 个
        // draft（active 豁免），auto/ 下共 21 个文件。
        {
            let mut sm = SkillManager::new(SkillExtractionConfig {
                skill_dir: skills_dir.clone(),
                ..Default::default()
            });
            for i in 0..22 {
                sm.create_distilled_skill(
                    &format!("Noise Skill {i:02}"),
                    "noise",
                    vec![],
                    Some("seed"),
                )
                .unwrap();
            }
        }
        let agent = build_agent(&config, root.clone(), provider, 5, None, vec![]).unwrap();
        let mut stream = agent
            .run_stream(deepseeknova_core::RunInput {
                prompt: "unrelated topic".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        let auto_dir = skills_dir.join("auto");
        let count = std::fs::read_dir(&auto_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .count();
        assert_eq!(
            count, 21,
            "会话边界应清理超额 draft：22 draft + 1 active(豁免) → 20 + 1 = 21，实得 {count}"
        );
        // active skill 未被清理
        assert!(auto_dir.join("fix-auth-flow.md").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 配置装配：`[memory] max_auto_draft_skills` 覆盖默认 20，且用户手写与
    /// verified 始终豁免清理。
    #[tokio::test]
    async fn skill_prune_honors_configured_max_auto_draft_skills() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;

        let root = std::env::temp_dir().join(format!("dnv-skill-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let skills_dir = root.join(".deepseeknova/skills");
        {
            let mut sm = SkillManager::new(SkillExtractionConfig {
                skill_dir: skills_dir.clone(),
                ..Default::default()
            });
            // 5 个 draft
            for i in 0..5 {
                sm.create_distilled_skill(&format!("Draft {i}"), "d", vec![], Some("seed"))
                    .unwrap();
            }
            // 1 个 verified（豁免）
            sm.create_distilled_skill("Keep Verified", "v", vec![], Some("seed"))
                .unwrap();
            for _ in 0..deepseeknova_core::memory::skill::VERIFY_USE_THRESHOLD {
                sm.record_use("keep-verified", true, Some("seed")).unwrap();
            }
            // 1 个用户手写（豁免）
            sm.create_skill(deepseeknova_core::memory::skill::Skill {
                frontmatter: deepseeknova_core::memory::skill::SkillFrontmatter {
                    name: "user-skill".into(),
                    version: "1.0.0".into(),
                    description: "user authored".into(),
                    triggers: vec![],
                    tags: vec![],
                    created_at: "2026-01-01T00:00:00Z".into(),
                    updated_at: "2026-01-01T00:00:00Z".into(),
                    use_count: 0,
                    success_count: 0,
                    source_session: None,
                },
                body: "b".into(),
            })
            .unwrap();
        }

        let mut config = Config::default();
        config.memory.enabled = true;
        config.memory.max_auto_draft_skills = 2; // 覆盖默认 20
        config.graph.enabled = false;
        config.review.enabled = false;
        config.verify.enabled = false;
        config.delegate.enabled = false;
        config.memory.llm_distill = false;
        let provider: Arc<dyn deepseeknova_provider::Provider> = Arc::new(stub_provider());
        let agent = build_agent(&config, root.clone(), provider, 5, None, vec![]).unwrap();
        let mut stream = agent
            .run_stream(deepseeknova_core::RunInput {
                prompt: "no match here".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        // auto/ 下：2 draft + 1 verified = 3；用户手写文件仍在根目录
        let auto_dir = skills_dir.join("auto");
        let count = std::fs::read_dir(&auto_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .count();
        assert_eq!(
            count, 3,
            "应只保留 2 个 draft + 1 个 verified，实得 {count}"
        );
        assert!(
            skills_dir.join("user-skill.md").exists(),
            "用户手写 skill 必须豁免"
        );
        let m = SkillManager::new(SkillExtractionConfig {
            skill_dir: skills_dir.clone(),
            ..Default::default()
        });
        assert_eq!(m.skill_state("keep-verified"), Some(SkillState::Verified));
        let _ = std::fs::remove_dir_all(&root);
    }

    // -----------------------------------------------------------------------
    // 协议增强能力包（阶段4）：失败模式回灌 / ingest 聚类 / fitness 记录
    // -----------------------------------------------------------------------

    /// 协议增强 §6.2：`[protocol] enabled=true` 且 store 有模式时，回灌注入
    /// 首轮 system prompt；≤3 条；enabled=false 时零注入。
    #[tokio::test]
    async fn failure_pattern_injection_injects_up_to_three() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;

        let root = std::env::temp_dir().join(format!("dsn-protocol-inj-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".deepseeknova/security")).unwrap();

        // 构造 store：4 条模式（count 4/3/2/1）。
        let mut store = deepseeknova_security::failure_pattern::FailurePatternStore::load(
            &root.join(".deepseeknova/security/failure-patterns.json"),
        )
        .unwrap();
        for (i, err) in ["err-a", "err-b", "err-c", "err-d"].iter().enumerate() {
            for _ in 0..(4 - i) {
                store.ingest("execute", Some("bash"), err, None, 1000 + i as u64);
            }
        }
        store.save().unwrap();

        // enabled=true：注入（捕获首轮 system prompt）。
        struct PromptCapture {
            system: Arc<std::sync::Mutex<Option<String>>>,
        }
        #[async_trait::async_trait]
        impl deepseeknova_provider::Provider for PromptCapture {
            async fn generate(
                &self,
                validated: deepseeknova_provider::ValidatedRequest<'_>,
            ) -> anyhow::Result<deepseeknova_core::Message> {
                *self.system.lock().unwrap() = validated
                    .messages
                    .iter()
                    .find(|m| m.role == deepseeknova_core::Role::System)
                    .map(|m| m.content.clone());
                Ok(deepseeknova_core::Message {
                    role: deepseeknova_core::Role::Assistant,
                    content: "ok".to_string(),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                })
            }
        }
        let mut config = Config::default();
        config.protocol.enabled = true;
        config.graph.enabled = false;
        config.memory.enabled = false;
        config.delegate.enabled = false;
        let capture = Arc::new(std::sync::Mutex::new(None));
        let provider: Arc<dyn deepseeknova_provider::Provider> = Arc::new(PromptCapture {
            system: capture.clone(),
        });
        let agent = attach_failure_pattern_injection(
            deepseeknova_agent::Agent::new(provider, 2),
            &config,
            &root,
        );
        let mut stream = agent
            .run_stream(deepseeknova_core::RunInput {
                prompt: "hi".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}
        let system = capture.lock().unwrap().clone().expect("system prompt set");
        assert!(system.contains("## 本会话已知失败模式（自动注入）"));
        // 3 条模式（top-3 by count），第 4 条不注入。
        assert_eq!(system.matches("- [失败模式]").count(), 3);
        assert!(system.contains("err-a") && system.contains("err-b") && system.contains("err-c"));
        assert!(
            !system.contains("err-d"),
            "4th pattern must not be injected"
        );

        // enabled=false：零注入。
        let mut config_off = Config::default();
        config_off.protocol.enabled = false;
        config_off.graph.enabled = false;
        config_off.memory.enabled = false;
        config_off.delegate.enabled = false;
        let capture_off = Arc::new(std::sync::Mutex::new(None));
        let provider_off: Arc<dyn deepseeknova_provider::Provider> = Arc::new(PromptCapture {
            system: capture_off.clone(),
        });
        let agent = attach_failure_pattern_injection(
            deepseeknova_agent::Agent::new(provider_off, 2),
            &config_off,
            &root,
        );
        let mut stream = agent
            .run_stream(deepseeknova_core::RunInput {
                prompt: "hi".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}
        let system_off = capture_off.lock().unwrap().clone().unwrap_or_default();
        assert!(
            !system_off.contains("本会话已知失败模式"),
            "protocol disabled must not inject"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 协议增强 §6.1：失败 run 结束后，diagnose hook 把 failures 聚类进
    /// failure-patterns.json（phase/tool/error/lesson 映射）；成功 run 不产生
    /// 模式文件（无 diagnose 报告）。enabled=false 时不写模式文件。
    #[tokio::test]
    async fn failure_pattern_ingest_clusters_from_diagnose() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;

        let root = std::env::temp_dir().join(format!("dsn-protocol-ingest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let metrics_dir = root.join(".deepseeknova/metrics");
        let patterns_path = root.join(".deepseeknova/security/failure-patterns.json");

        let mut config = Config::default();
        config.protocol.enabled = true;

        // 失败 run（EmptyProvider → MaxSteps → Paused → diagnose 报告）。
        let agent = attach_diagnose_hook_with_ingest(
            deepseeknova_agent::Agent::new(Arc::new(EmptyProvider), 2),
            metrics_dir.clone(),
            Some(&config),
            &root,
        );
        let mut stream = agent
            .run_stream(deepseeknova_core::RunInput {
                prompt: "hi".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}
        // 模式文件已生成且非空（failures 非空 → 至少 1 簇）。
        let store =
            deepseeknova_security::failure_pattern::FailurePatternStore::load(&patterns_path)
                .unwrap();
        assert!(
            !store.suggest(3).is_empty(),
            "failed run must cluster at least one pattern"
        );

        // enabled=false：不写模式文件。
        let root2 =
            std::env::temp_dir().join(format!("dsn-protocol-ingest-off-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root2);
        std::fs::create_dir_all(&root2).unwrap();
        let metrics_dir2 = root2.join(".deepseeknova/metrics");
        let patterns_path2 = root2.join(".deepseeknova/security/failure-patterns.json");
        let mut config_off = Config::default();
        config_off.protocol.enabled = false;
        let agent = attach_diagnose_hook_with_ingest(
            deepseeknova_agent::Agent::new(Arc::new(EmptyProvider), 2),
            metrics_dir2.clone(),
            Some(&config_off),
            &root2,
        );
        let mut stream = agent
            .run_stream(deepseeknova_core::RunInput {
                prompt: "hi".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}
        assert!(
            !patterns_path2.exists(),
            "protocol disabled must not create failure-patterns.json"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&root2);
    }

    /// 协议增强 §6.2：聚类入口 ingest 前对 failures 的 error/lesson 过
    /// `redact_secrets`——构造含密钥原文（AWS AKIA 键 + PEM 私钥头）的
    /// failures 直连 [`ingest_failure_patterns`]，断言落盘文件不含密钥原文、
    /// 只含 `[REDACTED]` 标记（接线侧最后防线；security 侧 ingest 入口另有
    /// 双保险）。
    #[test]
    fn failure_pattern_ingest_redacts_secrets_before_write() {
        use deepseeknova_agent::diagnose::FailureDetail;

        let root =
            std::env::temp_dir().join(format!("dsn-protocol-ingest-redact-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let patterns_path = root.join(".deepseeknova/security/failure-patterns.json");

        let aws_key = "AKIAIOSFODNN7EXAMPLE";
        let pem = "-----BEGIN RSA PRIVATE KEY-----";
        let failures = vec![
            FailureDetail {
                phase: "tool".into(),
                tool: Some("bash".into()),
                command: Some("aws s3 ls".into()),
                error: format!("Error: credentials {aws_key} rejected"),
                root_cause: Some(format!("env dump leaked {aws_key}")),
                fix_plan: None,
            },
            FailureDetail {
                phase: "plan".into(),
                tool: None,
                command: None,
                error: format!("config load failed: {pem}\nMIIEpAIBAAK..."),
                root_cause: None,
                fix_plan: Some(format!("rotate key material behind {pem}")),
            },
        ];
        let mut store =
            deepseeknova_security::failure_pattern::FailurePatternStore::load(&patterns_path)
                .unwrap();
        ingest_failure_patterns(&mut store, &failures, 1);
        store.save().unwrap();

        let text = std::fs::read_to_string(&patterns_path).unwrap();
        assert!(
            !text.contains(aws_key),
            "raw AWS key must not be persisted into failure-patterns.json"
        );
        assert!(
            !text.contains(pem),
            "raw PEM private key header must not be persisted"
        );
        assert!(
            text.contains("[REDACTED]"),
            "redacted marker must be persisted for secret-bearing failures"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 协议增强 §5：会话结束时按 outcome 记 fitness record_result 并落盘
    /// `<root>/.deepseeknova/skills/fitness.json`；会话技能名为空时跳过。
    #[tokio::test]
    async fn fitness_record_result_persists_on_session_end() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;

        let root = std::env::temp_dir().join(format!("dsn-protocol-fit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let fitness_path = root.join(".deepseeknova/skills/fitness.json");
        let metrics_dir = root.join(".deepseeknova/metrics");

        let mut config = Config::default();
        config.protocol.enabled = true;
        config.metrics.enabled = true;
        config.graph.enabled = false;
        config.memory.enabled = false;
        config.delegate.enabled = false;

        let ledger = Arc::new(deepseeknova_provider::cost::CostLedger::new());
        let session_skills = Arc::new(std::sync::Mutex::new(vec!["fix-auth".to_string()]));
        let agent = attach_metrics_hook_with_fitness(
            deepseeknova_agent::Agent::new(Arc::new(stub_provider()), 3),
            &config,
            MetricsSink {
                ledger,
                prices: Default::default(),
                dir: metrics_dir,
            },
            &root,
            session_skills,
        );
        let mut stream = agent
            .run_stream(deepseeknova_core::RunInput {
                prompt: "hi".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}
        // 成功 run：fitness.json 落盘且 success=1。
        let store = deepseeknova_skills::fitness::FitnessStore::load(&fitness_path).unwrap();
        let snap = store.snapshot();
        assert_eq!(snap.len(), 1, "one skill recorded");
        assert_eq!(snap[0].skill, "fix-auth");
        assert_eq!(snap[0].successes, 1);
        assert_eq!(snap[0].failures, 0);

        // 失败 run（EmptyProvider → Paused）：failures=1（同一技能）。
        let metrics_dir2 = root.join(".deepseeknova/metrics2");
        let session_skills2 = Arc::new(std::sync::Mutex::new(vec!["fix-auth".to_string()]));
        let agent2 = attach_metrics_hook_with_fitness(
            deepseeknova_agent::Agent::new(Arc::new(EmptyProvider), 2),
            &config,
            MetricsSink {
                ledger: Arc::new(deepseeknova_provider::cost::CostLedger::new()),
                prices: Default::default(),
                dir: metrics_dir2,
            },
            &root,
            session_skills2,
        );
        let mut stream = agent2
            .run_stream(deepseeknova_core::RunInput {
                prompt: "hi".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}
        let store = deepseeknova_skills::fitness::FitnessStore::load(&fitness_path).unwrap();
        let snap = store.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].successes, 1);
        assert_eq!(snap[0].failures, 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 协议增强 §5：空技能集（record_use 未接线，CLI 现状）时 hook 幂等
    /// 执行不 panic、fitness 文件不被写（save 不产生文件）；同一 hook 连续
    /// 两个会话可重复运行（warn-once 路径，第二次会话静默）。warn 噪音
    /// 本身不易断言（需挂 tracing subscriber），此测试守住行为面。
    #[tokio::test]
    async fn fitness_empty_skills_skips_silently_and_writes_no_file() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;

        let root =
            std::env::temp_dir().join(format!("dsn-protocol-fit-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let fitness_path = root.join(".deepseeknova/skills/fitness.json");
        let metrics_dir = root.join(".deepseeknova/metrics");

        let mut config = Config::default();
        config.protocol.enabled = true;
        config.metrics.enabled = true;
        config.graph.enabled = false;
        config.memory.enabled = false;
        config.delegate.enabled = false;

        let agent = attach_metrics_hook_with_fitness(
            deepseeknova_agent::Agent::new(Arc::new(stub_provider()), 3),
            &config,
            MetricsSink {
                ledger: Arc::new(deepseeknova_provider::cost::CostLedger::new()),
                prices: Default::default(),
                dir: metrics_dir,
            },
            &root,
            Arc::new(std::sync::Mutex::new(Vec::new())),
        );
        // 同一 hook 连续两个会话（warn-once 路径）：不 panic、不写文件。
        for _ in 0..2 {
            let mut stream = agent
                .run_stream(deepseeknova_core::RunInput {
                    prompt: "hi".into(),
                    images: Vec::new(),
                    model_override: None,
                })
                .await
                .unwrap();
            while stream.next().await.is_some() {}
        }
        assert!(
            !fitness_path.exists(),
            "empty session skills must not produce fitness.json"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 任务书 P 任务 2（spec §13 #9 接线）：recall 注入侧收集器 → session_skills
    /// → fitness record_use + record_result 全链路。预置技能文件，builder 传
    /// `Some(session_skills)`，run 后：收集器含注入技能名；fitness.json 出现
    /// 真实 use 记录（uses=1）与 result 记录（successes=1）；空集合场景（无
    /// 注入）由 `fitness_empty_skills_skips_silently_and_writes_no_file` 覆盖。
    #[tokio::test]
    async fn recall_injection_collects_skills_and_fitness_records_use_and_result() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;

        let root = std::env::temp_dir().join(format!("dsn-record-use-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // 预置用户技能：name 含 "auth"（强匹配），body 弱匹配兜底。
        let skills_dir = root.join(".deepseeknova/skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(
            skills_dir.join("fix-auth.md"),
            "---\nname: fix-auth\nversion: 1.0.0\ndescription: Fix authentication flows\ntags: [auth]\n---\nValidate tokens before trusting them.\n",
        )
        .unwrap();
        let fitness_path = skills_dir.join("fitness.json");
        let metrics_dir = root.join(".deepseeknova/metrics");

        let mut config = Config::default();
        config.protocol.enabled = true;
        config.metrics.enabled = true;
        config.memory.enabled = true;
        config.graph.enabled = false;
        config.delegate.enabled = false;
        config.verify.enabled = false;
        config.review.enabled = false;

        let session_skills: Arc<std::sync::Mutex<Vec<String>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let agent = build_agent_with_role_providers(
            &config,
            root.clone(),
            Arc::new(stub_provider()),
            AgentRoleProviders::default(),
            5,
            None,
            vec![],
            Some(session_skills.clone()),
        )
        .unwrap();
        let agent = attach_metrics_hook_with_fitness(
            agent,
            &config,
            MetricsSink {
                ledger: Arc::new(deepseeknova_provider::cost::CostLedger::new()),
                prices: Default::default(),
                dir: metrics_dir,
            },
            &root,
            session_skills.clone(),
        );
        // prompt 含 "auth"：起点召回（unseeded 首轮）匹配 fix-auth →
        // 注入 prompt → 收集器写入技能名。
        let mut stream = agent
            .run_stream(deepseeknova_core::RunInput {
                prompt: "auth".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        // 收集器含注入技能名。
        let collected = session_skills.lock().unwrap().clone();
        assert!(
            collected.iter().any(|s| s == "fix-auth"),
            "session_skills must contain injected skill, got {collected:?}"
        );
        // fitness.json：真实 use + result 记录（uses=1、successes=1）。
        assert!(
            fitness_path.exists(),
            "fitness.json must be written when skills were injected"
        );
        let store = deepseeknova_skills::fitness::FitnessStore::load(&fitness_path).unwrap();
        let snap = store.snapshot();
        assert_eq!(snap.len(), 1, "one skill recorded");
        assert_eq!(snap[0].skill, "fix-auth");
        assert_eq!(snap[0].uses, 1, "record_use must count the injection");
        assert_eq!(snap[0].successes, 1, "completed run → success");
        assert_eq!(snap[0].failures, 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 协议增强 §3.4：`[protocol] enabled=false`（默认）时 attach_protocol_gates
    /// 原样返回——run 事件流中不出现任何 PhaseTransition（protocol_active =
    /// gates 非空，零成本路径）。
    #[tokio::test]
    async fn protocol_gates_disabled_leaves_agent_unchanged() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;

        let mut config = Config::default();
        config.protocol.enabled = false;
        config.protocol.adversarial_review = false;
        let root = std::path::Path::new("");
        let agent = attach_protocol_gates(
            deepseeknova_agent::Agent::new(Arc::new(EmptyProvider), 2),
            &config,
            root,
        );
        let mut stream = agent
            .run_stream(deepseeknova_core::RunInput {
                prompt: "hi".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        while let Some(ev) = stream.next().await {
            let ev = ev.unwrap();
            assert!(
                !matches!(
                    ev,
                    deepseeknova_core::runner::RunEvent::PhaseTransition { .. }
                ),
                "protocol disabled must not emit phase events"
            );
        }
    }

    /// 协议增强 §3.4：enabled=true 时门注入（run 事件流出现 PhaseTransition
    /// 事件）；gates 配置解析（hard/soft/off + 非法值 warn 跳过）。
    #[tokio::test]
    async fn protocol_gates_enabled_injects_gates_and_parses_levels() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;
        use std::str::FromStr;

        // GateLevel 解析：合法三值 + 非法值报错（运行时 warn 跳过该项）。
        assert!(matches!(
            deepseeknova_agent::phase_runner::GateLevel::from_str("hard").unwrap(),
            deepseeknova_agent::phase_runner::GateLevel::Hard
        ));
        assert!(matches!(
            deepseeknova_agent::phase_runner::GateLevel::from_str("soft").unwrap(),
            deepseeknova_agent::phase_runner::GateLevel::Soft
        ));
        assert!(matches!(
            deepseeknova_agent::phase_runner::GateLevel::from_str("off").unwrap(),
            deepseeknova_agent::phase_runner::GateLevel::Off
        ));
        assert!(deepseeknova_agent::phase_runner::GateLevel::from_str("bogus").is_err());

        // builtin_phase_gates：缺省力度 + 覆盖 → 4 门。
        let mut levels: std::collections::HashMap<
            String,
            deepseeknova_agent::phase_runner::GateLevel,
        > = std::collections::HashMap::new();
        levels.insert(
            "verify-evidence".to_string(),
            deepseeknova_agent::phase_runner::GateLevel::from_str("hard").unwrap(),
        );
        let gates = deepseeknova_agent::phase_runner::builtin_phase_gates(&levels);
        assert_eq!(gates.len(), 4, "four builtin gates");

        // enabled=true（含一个非法 gate 值）→ run 产 PhaseTransition 事件。
        let mut config = Config::default();
        config.protocol.enabled = true;
        config
            .protocol
            .gates
            .insert("verify-evidence".to_string(), "hard".to_string());
        config
            .protocol
            .gates
            .insert("drift-detection".to_string(), "off".to_string());
        config
            .protocol
            .gates
            .insert("unknown-gate".to_string(), "bogus".to_string());
        let agent = attach_protocol_gates(
            deepseeknova_agent::Agent::new(Arc::new(EmptyProvider), 2),
            &config,
            std::path::Path::new(""),
        );
        let mut stream = agent
            .run_stream(deepseeknova_core::RunInput {
                prompt: "hi".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        let mut transitions = 0usize;
        while let Some(ev) = stream.next().await {
            if matches!(
                ev.unwrap(),
                deepseeknova_core::runner::RunEvent::PhaseTransition { .. }
            ) {
                transitions += 1;
            }
        }
        assert!(
            transitions >= 1,
            "protocol enabled must emit at least one phase transition, got {transitions}"
        );
    }

    /// 协议增强 §7：metrics hook 接线 fill_protocol——scorecard 落盘文件
    /// 含 protocol/composite 维（enabled=true 时 run 产阶段迁移 → 接线生效）。
    /// 数值语义（protocol_dim/composite_index 公式）由 metrics crate 单测
    /// 覆盖，此处验证 runtime 侧「compute 后覆写」接线存在。
    #[tokio::test]
    async fn scorecard_wires_protocol_dimension() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;

        let root = std::env::temp_dir().join(format!("dsn-protocol-card-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let metrics_dir = root.join(".deepseeknova/metrics");

        let mut config = Config::default();
        config.protocol.enabled = true;
        config.metrics.enabled = true;
        let ledger = Arc::new(deepseeknova_provider::cost::CostLedger::new());
        let agent = attach_metrics_hook_with_fitness(
            deepseeknova_agent::Agent::new(Arc::new(EmptyProvider), 2),
            &config,
            MetricsSink {
                ledger,
                prices: Default::default(),
                dir: metrics_dir.clone(),
            },
            &root,
            Arc::new(std::sync::Mutex::new(Vec::new())),
        );
        let mut stream = agent
            .run_stream(deepseeknova_core::RunInput {
                prompt: "hi".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        // 读 scorecard 落盘文件：protocol/composite 字段存在且值合法。
        let files: Vec<String> = std::fs::read_dir(&metrics_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".scorecard.json"))
            .collect();
        assert_eq!(files.len(), 1, "one scorecard written");
        let card: deepseeknova_metrics::Scorecard =
            serde_json::from_str(&std::fs::read_to_string(metrics_dir.join(&files[0])).unwrap())
                .unwrap();
        assert!(
            (0.0..=1.0).contains(&card.dimensions.protocol),
            "protocol dim out of range: {}",
            card.dimensions.protocol
        );
        assert!(
            (0.0..=1.0).contains(&card.dimensions.composite),
            "composite dim out of range: {}",
            card.dimensions.composite
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 协议增强 §7.1 末条：task_rate 双端接线之「成功端」——Completed 结束
    /// 无诊断报告（suppress），metrics hook 落盘前按 first_pass=true 填写；
    /// 评分卡 JSON 含 first_pass/retry_rounds 且值正确。
    #[tokio::test]
    async fn scorecard_task_rate_success_run_is_first_pass() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;

        let root = std::env::temp_dir().join(format!("dsn-taskrate-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let metrics_dir = root.join(".deepseeknova/metrics");

        let mut config = Config::default();
        config.protocol.enabled = true;
        config.metrics.enabled = true;
        let agent = attach_metrics_hook_with_fitness(
            deepseeknova_agent::Agent::new(Arc::new(stub_provider()), 3),
            &config,
            MetricsSink {
                ledger: Arc::new(deepseeknova_provider::cost::CostLedger::new()),
                prices: Default::default(),
                dir: metrics_dir.clone(),
            },
            &root,
            Arc::new(std::sync::Mutex::new(Vec::new())),
        );
        let mut stream = agent
            .run_stream(deepseeknova_core::RunInput {
                prompt: "hi".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        let files: Vec<String> = std::fs::read_dir(&metrics_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".scorecard.json"))
            .collect();
        assert_eq!(files.len(), 1, "one scorecard written");
        let card: deepseeknova_metrics::Scorecard =
            serde_json::from_str(&std::fs::read_to_string(metrics_dir.join(&files[0])).unwrap())
                .unwrap();
        assert!(
            card.first_pass,
            "success run must be first_pass=true, got {card:?}"
        );
        assert_eq!(card.retry_rounds, 0);
        // 无诊断报告（suppress）→ 诊断回调不触发，task_rate 不被覆写。
        let diag_dir = metrics_dir.join("diagnose");
        assert!(
            !diag_dir.exists(),
            "success run must not write diagnose dir"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 协议增强 §7.1 末条：task_rate 双端接线之「失败端」——Paused 路径
    /// metrics hook 先触发（评分卡按保守 false/0 落盘），诊断回调随后按
    /// failures 覆写：first_pass=false、retry_rounds=failures 条数（≥1）。
    #[tokio::test]
    async fn scorecard_task_rate_failed_run_backfilled_from_diagnose() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;

        let root = std::env::temp_dir().join(format!("dsn-taskrate-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let metrics_dir = root.join(".deepseeknova/metrics");

        let mut config = Config::default();
        config.protocol.enabled = true;
        config.metrics.enabled = true;
        config.graph.enabled = false;
        config.memory.enabled = false;
        config.delegate.enabled = false;
        let agent = attach_metrics_hook_with_fitness(
            deepseeknova_agent::Agent::new(Arc::new(EmptyProvider), 2),
            &config,
            MetricsSink {
                ledger: Arc::new(deepseeknova_provider::cost::CostLedger::new()),
                prices: Default::default(),
                dir: metrics_dir.clone(),
            },
            &root,
            Arc::new(std::sync::Mutex::new(Vec::new())),
        );
        // CLI 装配顺序：metrics → quality → diagnose → failure pattern → gates。
        // 此处按同序挂 diagnose（task_rate 回填依赖评分卡先落盘）。
        let agent =
            attach_diagnose_hook_with_ingest(agent, metrics_dir.clone(), Some(&config), &root);
        let mut stream = agent
            .run_stream(deepseeknova_core::RunInput {
                prompt: "hi".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        // 诊断报告存在（Paused）且 failures 非空。
        let diag_dir = metrics_dir.join("diagnose");
        let diag_files: Vec<String> = std::fs::read_dir(&diag_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".json"))
            .collect();
        assert_eq!(diag_files.len(), 1, "one diagnose report written");
        let report: deepseeknova_agent::diagnose::DiagnoseReport =
            serde_json::from_str(&std::fs::read_to_string(diag_dir.join(&diag_files[0])).unwrap())
                .unwrap();
        assert!(!report.failures.is_empty(), "paused run must have failures");

        // 评分卡 task_rate 被诊断回调覆写为真实值。
        let files: Vec<String> = std::fs::read_dir(&metrics_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".scorecard.json"))
            .collect();
        assert_eq!(files.len(), 1, "one scorecard written");
        let card: deepseeknova_metrics::Scorecard =
            serde_json::from_str(&std::fs::read_to_string(metrics_dir.join(&files[0])).unwrap())
                .unwrap();
        assert!(!card.first_pass, "paused run must not be first_pass");
        assert_eq!(
            card.retry_rounds as usize,
            report.failures.len(),
            "retry_rounds must equal diagnose failures count"
        );
        assert!(card.retry_rounds >= 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// P-L2：task_rate 回填仅限失败型会话——Cancelled 且零失败详情的会话
    /// 不得被诊断回填覆写为 first_pass=true，保持 metrics hook 已填的保守
    /// false/0（非 Completed 路径）；失败型会话（failures 非空）仍覆写。
    #[test]
    fn diagnose_backfill_keeps_first_pass_for_zero_failure_reports() {
        let root = std::env::temp_dir().join(format!("dsn-taskrate-zero-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let write_card = |session_id: &str| {
            let stats = deepseeknova_metrics::SessionStats {
                tool_calls: 1,
                ..Default::default()
            };
            let card = deepseeknova_metrics::Scorecard::compute(session_id, &stats, &[], 0, 0, 0);
            deepseeknova_metrics::write_scorecard(&card, &root).unwrap();
        };
        let read_card = |session_id: &str| -> deepseeknova_metrics::Scorecard {
            serde_json::from_str(
                &std::fs::read_to_string(root.join(format!("{session_id}.scorecard.json")))
                    .unwrap(),
            )
            .unwrap()
        };

        // Cancelled 零失败会话：metrics hook 已落保守 false/0，回填不得覆写。
        write_card("s-cancelled");
        let cancelled =
            deepseeknova_agent::diagnose::DiagnoseReport::new("s-cancelled", "cancelled");
        backfill_scorecard_task_rate(&root, &cancelled);
        assert!(
            !read_card("s-cancelled").first_pass,
            "Cancelled 零失败会话不得被标 first_pass=true"
        );
        assert_eq!(read_card("s-cancelled").retry_rounds, 0);

        // 失败型会话（failures 非空）仍覆写 first_pass=false + 条数。
        write_card("s-fail");
        let mut failed = deepseeknova_agent::diagnose::DiagnoseReport::new("s-fail", "paused");
        failed
            .failures
            .push(deepseeknova_agent::diagnose::FailureDetail {
                phase: "tool".into(),
                tool: None,
                command: None,
                error: "boom".into(),
                root_cause: None,
                fix_plan: None,
            });
        backfill_scorecard_task_rate(&root, &failed);
        let back = read_card("s-fail");
        assert!(!back.first_pass);
        assert_eq!(back.retry_rounds, 1);

        let _ = std::fs::remove_dir_all(&root);
    }
}
