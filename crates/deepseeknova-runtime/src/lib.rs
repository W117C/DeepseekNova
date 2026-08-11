//! # Runtime — Composition root
//!
//! Wires together all DeepseekNova subsystems: registry, context, event bus,
//! permission, security, and LLM provider into a ready-to-use agent runtime.
//!
//! M7b：大型装配函数已按主题拆分为子模块（security/metrics/hooks/diagnose/
//! protocol/delegate/helpers），组合根 `build_agent_with_role_providers` 与
//! `Runtime` 保留在此，对外 API 经 `pub use` 原样再导出。

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        clippy::dbg_macro
    )
)]

mod delegate;
mod diagnose;
mod helpers;
mod hooks;
mod mention;
mod metrics;
mod protocol;
mod security;
#[cfg(test)]
mod test_support;

use std::path::PathBuf;
use std::sync::Arc;

use deepseeknova_config::Config;
use deepseeknova_context::ContextProvider;
use deepseeknova_core::registry::RegistryHub;
use deepseeknova_core::runner::{RunEventStream, RunInput, Runner};
use deepseeknova_event::EventBus;
use deepseeknova_permission::PermissionGate;

// 组合根内部引用的私有装配函数（跨子模块）。
use delegate::build_delegate_engine;
use helpers::{derive_compaction_threshold, repo_map_seeds, run_blocking_work};
use security::sandbox_writable_paths;

// ── 对外 API 再导出：被 CLI 等外部 crate 消费的装配函数保持 crate 根路径 ──
pub use delegate::{build_sub_agent_runner, delegate_agent_names};
pub use diagnose::{
    attach_diagnose_hook, attach_diagnose_hook_with_ingest, attach_failure_pattern_injection,
};
pub use hooks::{attach_quality_hook, attach_user_hooks};
pub use mention::MentionAwareRunner;
pub use metrics::{
    attach_metrics_hook, attach_metrics_hook_with_fitness, enforce_metrics_retention, MetricsSink,
};
pub use protocol::attach_protocol_gates;
pub use security::{build_permission_gate, build_security_context, permission_gate_for};
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

/// Runtime is the composition root. It wires registry, context, events,
/// and permission together. Agent, Planner, SubAgent, Server all share
/// one Runtime.
pub struct Runtime {
    /// 工具/规划器注册表（读写锁保护）。
    pub registry: Arc<std::sync::RwLock<RegistryHub>>,
    /// 上下文提供者（workspace 索引 / prompt 构建）。
    pub context: Arc<dyn ContextProvider>,
    /// 事件总线（运行时事件发布/订阅）。
    pub events: Arc<EventBus>,
    /// 权限门（工具调用审批决策）。
    pub permission: Arc<PermissionGate>,
    /// 运行时配置。
    pub config: Arc<Config>,
}

impl Runtime {
    /// Create a Runtime with a given context provider.
    pub fn new(
        config: Config,
        context: Arc<dyn ContextProvider>,
    ) -> Result<Self, deepseeknova_core::DeepseeknovaError> {
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
    ) -> Result<RunEventStream, deepseeknova_core::DeepseeknovaError> {
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
) -> Result<deepseeknova_agent::Agent, deepseeknova_core::DeepseeknovaError> {
    // 提前克隆供 P2 段使用（task/compact/review 字段在下方会被移动）。
    let step_quick = roles.step_quick.clone();
    let step_high = roles.step_high.clone();
    let observe_provider = roles.compact.clone().or_else(|| step_quick.clone());
    let security = build_security_context(config, &workspace_root)?;

    // ── 默认安全姿态横幅：任一深度防御层未生效即在启动日志明示。仅提示、
    // 不改变运行；与 Windows 无 OS 沙箱后端的 main.rs 警告语义一致。
    if !config.permissions.enabled {
        tracing::warn!(
            "⚠ security posture reduced: permission gate DISABLED \
             (default ON since B3; set [permissions] enabled=true to enforce)"
        );
    }
    if !config.sandbox.enabled {
        tracing::warn!(
            "⚠ security posture reduced: sandbox DISABLED \
             (set [sandbox] enabled=true or run with --secure-defaults)"
        );
    }

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

    // Ask 无 responder 兜底：默认 fail-closed（deny），与子代理侧
    // （sub_agent.rs Ask 一律视作拒绝）语义对齐；显式
    // `ask_without_responder = "allow"` 恢复旧的自动放行契约。
    agent = agent.with_ask_without_responder_deny(
        config.permissions.ask_without_responder == deepseeknova_config::AskFallback::Deny,
    );

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

    // delegate 工具（原属 tools crate，移入 agent crate 以消除反向依赖）。
    // 与内置工具同级注册；引擎句柄经 extension 注入（见上方 delegate 段），
    // 缺失时工具优雅降级（返回未启用提示）。
    register(&mut agent, vec![Arc::new(deepseeknova_agent::DelegateTool)]);

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

    // 技能工具：三层来源（builtin / user / project）按 scope 优先级解析，注册为
    // `skill__<name>` 工具。可经 tools.overrides 按名禁用（精确匹配 skill__<name>）。
    // 用户级目录 `~/.deepseeknova/skills`，项目级 `.deepseeknova/skills`（后加者
    // 覆盖 `.agents/skills`）。
    {
        use deepseeknova_core::registry::SkillScope;
        use deepseeknova_skills::{SkillResolver, SkillTool};
        let user_skills = dirs::home_dir()
            .map(|h| h.join(".deepseeknova/skills"))
            .unwrap_or_default();
        let resolver = SkillResolver::new()
            .add_preloaded(
                SkillScope::Builtin,
                deepseeknova_skills::load_builtin_skills(),
            )
            .add_source(SkillScope::User, user_skills)
            .add_source(SkillScope::Project, ".deepseeknova/skills")
            .add_source(SkillScope::Project, ".agents/skills");
        let skill_tools: Vec<Arc<dyn deepseeknova_core::Tool>> = resolver
            .resolve()
            .into_iter()
            .map(|s| Arc::new(SkillTool::new(s)) as Arc<dyn deepseeknova_core::Tool>)
            .collect();
        if !skill_tools.is_empty() {
            register(&mut agent, skill_tools);
        }
    }

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
        // P2-2 语义检索接线：复用记忆侧 embedder（`[memory] embedder = "remote"`）。
        // 缺 key/网络错 fail-open 回落纯 FTS（try_memory_embedder 返回 None 时
        // open_with_embedder 等价 open）。
        let embedder = deepseeknova_provider::embeddings::try_memory_embedder(&config.memory);
        match deepseeknova_graph::GraphIndex::open_with_embedder(
            &workspace_root,
            config.graph.max_file_size,
            embedder,
            "graph-embed",
        ) {
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

                // Feed a repo map into the agent's system prompt at run start.
                // Personalization seeds are extracted from the user query via
                // repo_map_seeds() in the provider closure below.
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
                            // H4：起点召回含同步 embed（remote embedder 的 HTTP
                            // block_on），经 run_blocking_work 释放 tokio worker。
                            run_blocking_work(|| {
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
                            })
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
                                // H4：中途召回含同步 embed（remote embedder 的
                                // HTTP block_on），经 run_blocking_work 释放 worker。
                                run_blocking_work(|| {
                                    let mut block = String::new();
                                    let mut budget = mid_cap;
                                    if let Ok(hits) = mid_mem.recall_with_weight(
                                        query,
                                        mid_top_k,
                                        mid_rank_weight,
                                    ) {
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
                                            if let Ok(nodes) =
                                                idx.search(query, None, mid_graph_top_k)
                                            {
                                                let mut lines: Vec<String> = Vec::new();
                                                for n in nodes {
                                                    let line = format!(
                                                        "- [graph] {} ({})\n",
                                                        n.name, n.path
                                                    );
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
                                })
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
                    // H4：record_task 含同步 embed（最多约 22 次 HTTP block_on），
                    // 经 run_blocking_work 释放 tokio worker。
                    run_blocking_work(|| {
                        if let Err(e) = dh.record_task(&obs, &guards) {
                            tracing::warn!("memory distill failed: {e}");
                        }
                    });
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
        let handle: deepseeknova_agent::DelegateHandle = engine;
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

    // ── 用户级外部 hooks（`[hooks]` 段）：enabled 且非空时挂载；否则原样返回 ──
    agent = attach_user_hooks(agent, config);

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
) -> Result<deepseeknova_agent::Agent, deepseeknova_core::DeepseeknovaError> {
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
/// - always injects the [`deepseeknova_security::context::SecurityContext`] (capabilities, path confinement,
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
) -> Result<deepseeknova_agent::Agent, deepseeknova_core::DeepseeknovaError> {
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

// 组合根集成测试（build_agent* / Runtime / MCP 发现）已随 P2-D 拆分迁至
// 同目录 tests.rs（纯行范围搬移，无内容变更），本文件仅保留模块声明。
#[cfg(test)]
mod tests;
