//! # Runtime — Composition root
//!
//! Wires together all DeepseekNova subsystems: registry, context, event bus,
//! permission, security, and LLM provider into a ready-to-use agent runtime.
//!
//! M7b：大型装配函数已按主题拆分为子模块（security/metrics/hooks/diagnose/
//! protocol/delegate/helpers），组合根 `build_agent_with_role_providers` 与
//! `Runtime` 保留在此，对外 API 经 `pub use` 原样再导出。

mod delegate;
mod diagnose;
mod helpers;
mod hooks;
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
pub use delegate::build_sub_agent_runner;
pub use diagnose::{
    attach_diagnose_hook, attach_diagnose_hook_with_ingest, attach_failure_pattern_injection,
};
pub use hooks::{attach_quality_hook, attach_user_hooks};
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
) -> anyhow::Result<deepseeknova_agent::Agent> {
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
#[cfg(test)]
mod tests {
    // 组合根集成测试：主 agent 构建（build_agent*）、Runtime、MCP 发现等。
    use super::*;
    use crate::test_support::*;
    use deepseeknova_config::Config;
    use deepseeknova_context::ContextEngine;
    use deepseeknova_core::memory::skill::{SkillExtractionConfig, SkillManager, SkillState};

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

    /// H4 端到端回归：remote embedder 配置下，起点召回闭包内的同步 embed
    /// （真实 HTTP 往返，服务端延迟 500ms）不得阻塞 tokio worker。
    /// 断言：embed 阻塞窗口 [server_started, server_responded] 内心跳必须
    /// 持续推进（证明 worker 已被 block_in_place 释放）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn recall_embed_does_not_starve_the_worker_thread() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Mutex;

        // 本地一次性 embed 服务：请求到达 → 记录时间 → 延迟 500ms → 回复。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server_started = Arc::new(Mutex::new(None::<std::time::Instant>));
        let server_responded = Arc::new(Mutex::new(None::<std::time::Instant>));
        {
            let (ss, sr) = (server_started.clone(), server_responded.clone());
            std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                // 读至 headers 结束（\r\n\r\n）；单条小 body 随 headers 同包到达。
                while buf.windows(4).all(|w| w != b"\r\n\r\n") {
                    let n = stream.read(&mut tmp).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                *ss.lock().unwrap() = Some(std::time::Instant::now());
                std::thread::sleep(std::time::Duration::from_millis(500));
                *sr.lock().unwrap() = Some(std::time::Instant::now());
                let body = r#"{"object":"list","data":[{"object":"embedding","index":0,"embedding":[0.1,0.2,0.3]}]}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            });
        }
        let base = format!("http://{addr}/v1");

        // 保存/恢复 embed key 环境变量（remote embedder 装配必需）。
        let prev_key = std::env::var("DEEPSEEKNOVA_EMBED_API_KEY").ok();
        std::env::set_var("DEEPSEEKNOVA_EMBED_API_KEY", "sk-h4-test");
        let restore_env = || match &prev_key {
            Some(v) => std::env::set_var("DEEPSEEKNOVA_EMBED_API_KEY", v),
            None => std::env::remove_var("DEEPSEEKNOVA_EMBED_API_KEY"),
        };

        let root = std::env::temp_dir().join(format!("dnv-h4-embed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut config = Config::default();
        config.memory.embedder = "remote".into();
        config.memory.embed_model = "test-model".into();
        config.memory.embed_base_url = base;

        let agent = build_agent(
            &config,
            root.clone(),
            std::sync::Arc::new(stub_provider()),
            5,
            None,
            vec![],
        )
        .expect("build_agent with remote embedder should succeed");

        // 心跳：2ms 周期记录 tick 时间戳。
        let ticks = Arc::new(Mutex::new(Vec::<std::time::Instant>::new()));
        let stop = Arc::new(AtomicBool::new(false));
        {
            let (tk, st) = (ticks.clone(), stop.clone());
            let heartbeat = tokio::spawn(async move {
                while !st.load(Ordering::SeqCst) {
                    tk.lock().unwrap().push(std::time::Instant::now());
                    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                }
            });
            // run 起点召回触发 embed（HTTP 500ms 阻塞窗口）。
            let mut stream = agent
                .run_stream(deepseeknova_core::RunInput {
                    prompt: "h4 run".into(),
                    images: Vec::new(),
                    model_override: None,
                })
                .await
                .unwrap();
            while stream.next().await.is_some() {}
            stop.store(true, Ordering::SeqCst);
            heartbeat.await.unwrap();
        }

        restore_env();
        let start = server_started
            .lock()
            .unwrap()
            .expect("embed request must arrive");
        let end = server_responded
            .lock()
            .unwrap()
            .expect("embed must respond");
        let all_ticks = ticks.lock().unwrap().clone();
        let in_window = all_ticks.iter().any(|t| t >= &start && t <= &end);
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            in_window,
            "embed 阻塞窗口 {start:?}..{end:?} 内无心跳（{} 个 tick 均在外）：worker 被占用",
            all_ticks.len()
        );
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
    fn graph_retrieval_hint_stays_english_and_graph_first() {
        for tool in ["search_code", "traverse_graph", "retrieve_entity"] {
            assert!(GRAPH_RETRIEVAL_HINT.contains(tool), "hint missing {tool}");
        }
        assert!(
            !GRAPH_RETRIEVAL_HINT.contains("检索"),
            "hint must be English, not Chinese"
        );
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

    #[tokio::test]
    async fn attach_user_hooks_fires_session_start_end_to_end() {
        use futures::StreamExt;
        let mut config = Config::default();
        config.graph.enabled = false;
        config.memory.enabled = false;
        config.verify.enabled = false;
        config.review.enabled = false;
        let root = std::env::temp_dir().join(format!("dnv-hooks-session-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let markers = root.join("session.log");
        config.hooks = deepseeknova_config::HooksConfig {
            enabled: true,
            session_start: vec![marker_cmd("start", &markers)],
            ..Default::default()
        };
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
        let mut stream = agent
            .run_stream(RunInput {
                prompt: "hi".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}
        let text = std::fs::read_to_string(&markers).unwrap_or_default();
        assert!(
            text.contains("start"),
            "build_agent 装配的 session_start hook 必须触发: {text:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn attach_user_hooks_noop_when_disabled() {
        use futures::StreamExt;
        // enabled=false：即便配置了命令也不挂载（零开销，不 spawn 进程）。
        let mut config = Config::default();
        config.graph.enabled = false;
        config.memory.enabled = false;
        config.verify.enabled = false;
        config.review.enabled = false;
        let root = std::env::temp_dir().join(format!("dnv-hooks-disabled-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let markers = root.join("session.log");
        config.hooks = deepseeknova_config::HooksConfig {
            enabled: false,
            session_start: vec![marker_cmd("start", &markers)],
            ..Default::default()
        };
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
        let mut stream = agent
            .run_stream(RunInput {
                prompt: "hi".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}
        assert!(
            !markers.exists(),
            "hooks 关闭时不得触发任何外部命令（零开销）"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // -----------------------------------------------------------------------
    // M8b：builder disabled-set 过滤补全 + 预算/验证接线
    // -----------------------------------------------------------------------

    /// 记忆关闭时必须把所有记忆工具（remember/recall/forget）从注册表剔除，
    /// 模型看不到其 schema（与 graph 同款处理）。既有测试只查 recall，
    /// 这里补 remember/forget 全覆盖。
    #[test]
    fn build_agent_skips_all_memory_tools_when_disabled() {
        let mut config = Config::default();
        config.memory.enabled = false;
        config.graph.enabled = false;
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
        let names = agent.tool_names();
        for tool in ["remember", "recall", "forget"] {
            assert!(
                !names.iter().any(|n| n == tool),
                "{tool} 必须在 memory 关闭时被排除，实得: {names:?}"
            );
        }
    }

    #[tokio::test]
    async fn build_agent_registers_remember_and_forget_when_memory_enabled() {
        let mut config = Config::default();
        config.memory.enabled = true;
        config.graph.enabled = false;
        let root = std::env::temp_dir().join(format!("dnv-mem-forget-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, root.clone(), provider, 5, None, vec![]).unwrap();
        let names = agent.tool_names();
        for tool in ["remember", "recall", "forget"] {
            assert!(
                names.iter().any(|n| n == tool),
                "{tool} 必须在 memory 开启时注册，实得: {names:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// tools.overrides 对内置工具同样生效：禁用 web_search 后模型看不到其
    /// schema（与既有 extra_tools 覆盖同款 disabled-set 过滤）。
    #[test]
    fn build_agent_disables_builtin_tool_via_overrides() {
        let mut config = Config::default();
        config.graph.enabled = false;
        config.memory.enabled = false;
        config.tools.overrides = vec![deepseeknova_config::ToolOverride {
            name: "web_search".into(),
            disabled: true,
            timeout_secs: None,
            max_file_size: None,
        }];
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
        let names = agent.tool_names();
        assert!(
            !names.iter().any(|n| n == "web_search"),
            "web_search 必须被 overrides 禁用，实得: {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "read_file"),
            "其他内置工具不受影响"
        );
    }

    /// B2 预算接线：`[budget] enabled=true` 时运行时挂 PromptBudgetController。
    /// 极小 system prompt + 极低 max_total_tokens → 首步预算 Reject → 优雅
    /// Paused（reason 含 "budget"），证明预算守门在生产路径真实生效。
    #[tokio::test]
    async fn build_agent_wires_token_budget_and_pauses_on_excess() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;
        let mut config = Config::default();
        config.agent.system_prompt = Some("tiny".into()); // 极小 system prompt
        config.budget.enabled = true;
        config.budget.max_total_tokens = 64;
        config.budget.max_memory_tokens = 16;
        config.graph.enabled = false;
        config.memory.enabled = false;
        config.delegate.enabled = false;
        config.verify.enabled = false;
        config.review.enabled = false;
        config.agent.l3_compaction = false;
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
        let mut stream = agent
            .run_stream(RunInput {
                prompt: "hi".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        let mut paused_reason: Option<String> = None;
        while let Some(ev) = stream.next().await {
            if let Ok(deepseeknova_core::runner::RunEvent::Paused { reason, .. }) = ev {
                paused_reason = Some(reason);
            }
        }
        assert!(
            paused_reason.is_some(),
            "budget Reject 必须 Paused（而非跑满 max_steps）"
        );
        assert!(
            paused_reason
                .as_deref()
                .unwrap_or_default()
                .contains("budget"),
            "Paused reason 必须说明预算: {paused_reason:?}"
        );
    }

    /// P2-4 团队级花费上限接线：build_agent 后叠加 `with_cost_budget`（CLI
    /// 同款装配），账本已超限时首步即 Paused（reason 含 "cost"）。
    #[tokio::test]
    async fn build_agent_wires_cost_budget_pausing_on_exceeded_spend() {
        use deepseeknova_core::Runner;
        use deepseeknova_provider::cost::{CostLedger, ModelPrices, ModelRole};
        use futures::StreamExt;

        // 预置账本：模型 "big" 有完整单价，1M prompt → 2.0 USD ≥ 上限 1.0。
        let ledger = Arc::new(CostLedger::new());
        let mut prices = deepseeknova_provider::cost::PriceTable::new();
        prices.insert(
            "big".to_string(),
            ModelPrices {
                input_per_mtok: Some(2.0),
                output_per_mtok: Some(8.0),
                cache_hit_per_mtok: Some(0.2),
            },
        );
        ledger.record(
            ModelRole::Main,
            "big",
            &deepseeknova_core::chunk::Usage {
                prompt_tokens: 1_000_000,
                completion_tokens: 0,
                total_tokens: 1_000_000,
                cache_hit_tokens: 0,
                cache_miss_tokens: 1_000_000,
                reasoning_tokens: 0,
            },
        );

        let mut config = Config::default();
        config.graph.enabled = false;
        config.memory.enabled = false;
        config.delegate.enabled = false;
        config.verify.enabled = false;
        config.review.enabled = false;
        let agent = build_agent(
            &config,
            std::env::temp_dir(),
            Arc::new(stub_provider()),
            5,
            None,
            vec![],
        )
        .unwrap()
        .with_cost_budget(deepseeknova_agent::budget::cost::CostBudget::new(
            ledger, prices, 1.0,
        ));
        let mut stream = agent
            .run_stream(RunInput {
                prompt: "hi".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        let mut paused_reason: Option<String> = None;
        while let Some(ev) = stream.next().await {
            if let Ok(deepseeknova_core::runner::RunEvent::Paused { reason, .. }) = ev {
                paused_reason = Some(reason);
            }
        }
        assert!(paused_reason.is_some(), "成本超限必须 Paused");
        assert!(
            paused_reason
                .as_deref()
                .unwrap_or_default()
                .contains("cost"),
            "Paused reason 必须指出成本上限: {paused_reason:?}"
        );
    }

    /// P4 验证接线：`[verify] enabled=true` + commands 非空时装配验证链
    /// （构建不 panic）；llm=false 不要求额外 provider 解析。
    #[test]
    fn build_agent_wires_verify_with_command() {
        let mut config = Config::default();
        config.graph.enabled = false;
        config.memory.enabled = false;
        config.verify.enabled = true;
        config.verify.commands = vec!["echo ok".into()];
        config.verify.llm = false;
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
        let _ = agent;
    }

    /// disabled-set 过滤与 graph 开关叠加：graph 启用但 tools.overrides
    /// 禁用 search_code 时，该工具仍被剔除（overrides 与功能开关两路
    /// disabled 集合合并，模型看不到被禁 schema）。
    #[tokio::test]
    async fn build_agent_disables_graph_tool_via_overrides_even_when_graph_enabled() {
        let mut config = Config::default();
        config.graph.enabled = true;
        config.memory.enabled = false;
        config.tools.overrides = vec![deepseeknova_config::ToolOverride {
            name: "search_code".into(),
            disabled: true,
            timeout_secs: None,
            max_file_size: None,
        }];
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
        let names = agent.tool_names();
        assert!(
            !names.iter().any(|n| n == "search_code"),
            "overrides 禁用必须叠加到 graph 启用的工具集，实得: {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "traverse_graph"),
            "其他图工具仍保留"
        );
    }

    /// A1 检查点接线：`[checkpoint] enabled=true` 时装配 CheckpointManager
    ///（构建不 panic；缺文件时 warn 后新建，行为与默认一致）。
    #[test]
    fn build_agent_wires_checkpoint_when_enabled() {
        let mut config = Config::default();
        config.checkpoint.enabled = true;
        config.graph.enabled = false;
        config.memory.enabled = false;
        let root = std::env::temp_dir().join(format!("dnv-cp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, root.clone(), provider, 5, None, vec![]).unwrap();
        let _ = agent;
        let _ = std::fs::remove_dir_all(&root);
    }
}
