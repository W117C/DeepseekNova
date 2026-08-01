use crate::memory::Memory;
use deepseeknova_core::chunk::{Chunk, Usage};
use deepseeknova_core::memory::skill::{TaskObservation, TaskOutcome};
use deepseeknova_core::tool::ToolContext;
use deepseeknova_core::types::{FunctionCall, ToolCall};
use deepseeknova_core::{
    Message, Role, RunEvent, RunEventStream, RunInput, RunOutput, Runner, Tool,
};
use deepseeknova_permission::{Decision, PermissionGate};
use deepseeknova_provider::Provider;
use deepseeknova_security::context::SecurityContext;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

// Re-export the approval trait (defined in core, next to `RunEvent`) so
// existing `deepseeknova_agent::ApprovalResponder` references keep resolving.
pub use deepseeknova_core::runner::ApprovalResponder;

// P3.1 统一 CJK-aware token 估算（实现见 tokens.rs，本文件仅 re-export）。
pub use crate::tokens::estimate_tokens;

// ---------------------------------------------------------------------------
// Agent — the main agent runner
// ---------------------------------------------------------------------------

pub struct Agent {
    provider: Arc<dyn Provider>,
    tools: HashMap<String, Arc<dyn Tool>>,
    max_steps: usize,
    system_prompt: Option<String>,
    /// Workspace root used to confine filesystem tool calls. Defaults to the
    /// process working directory at construction time.
    workspace_root: PathBuf,
    /// Security context injected into every ToolContext. Defaults to the
    /// safe-defaults policy (all builtin capabilities granted).
    security: SecurityContext,

    compaction_threshold_tokens: Option<u32>,

    /// Optional persistent conversation store. When set, each `run_stream`
    /// seeds its working memory from this store at the start and writes the
    /// full conversation back at the end, giving the agent multi-turn memory
    /// across separate `run_stream` invocations. This is what enables desktop
    /// / CLI sessions to carry context — and crucially, it lets DeepSeek-V4's
    /// `reasoning_content` replay contract span user turns, not just the
    /// tool-loop within a single run.
    history: Option<Arc<tokio::sync::Mutex<Vec<Message>>>>,

    /// Optional permission gate. When set, every tool call is checked before
    /// execution: Allow → run, Deny → blocked, Ask → routed to the approval
    /// responder. When `None` (the default), tools run unconditionally.
    permission: Option<Arc<PermissionGate>>,

    /// Optional approval responder used to resolve `Ask` decisions.
    approval: Option<Arc<dyn ApprovalResponder>>,

    /// Build-time registered extensions injected into every ToolContext.
    /// Stored as closures to erase the concrete type while staying Clone-free.
    extensions: Vec<Arc<ExtensionApplier>>,

    /// Optional repo-map provider. When set, `run_stream` invokes it at the
    /// start of a fresh conversation to produce a code-graph "repo map" that is
    /// appended to the system prompt (stable prefix region). Returns `None`
    /// when no map is available (e.g. empty budget or index unavailable). This
    /// is wired by the runtime from a shared `GraphHandle` + token budget.
    repo_map_provider: Option<RepoMapProvider>,

    recall_provider: Option<RecallProvider>,
    /// P3.3 中途检索（续聊开头 / 压缩后注入记忆 + 代码图命中）。
    mid_run: Option<MidRunRetrieval>,
    distill_hook: Option<DistillHook>,

    /// max_steps 到顶行为：true = 优雅暂停（默认），false = 旧版报错。
    pause_on_max_steps: bool,

    /// L3 结构化压缩开关（config.agent.l3_compaction）。
    l3_enabled: bool,

    /// L3 摘要用 provider；None = 复用主 provider。
    compact_provider: Option<Arc<dyn Provider>>,

    /// step 边界预算守门；None = 关闭。
    budget: Option<crate::budget::controller::PromptBudgetController>,

    /// 暂停事件附带的会话标注（CLI/desktop 持久化开启时注入）。
    session_label: Option<String>,

    /// B3 审查：provider + 设置；None = 关闭（默认）。
    review_provider: Option<Arc<dyn Provider>>,
    review_settings: Option<crate::review::ReviewSettings>,

    /// 审查计数钩子（runtime 注入，落 memory counters；None = 仅 tracing）。
    review_counter: Option<ReviewCounterHook>,

    /// 同批工具调用是否允许并发执行（读类并发、写类保序串行）。
    concurrent_tools: bool,

    /// P1 确定性验证设置；None = 关闭（默认）。
    verify_settings: Option<crate::verify::VerifySettings>,

    /// P2 每步 effort 路由（quick=thinking off / high=高推理）；None = 固定主 provider。
    effort_routing: Option<EffortRouting>,

    /// P2 观察压缩设置；None = 关闭（默认）。
    observe: Option<ObserveSettings>,

    /// P2 会话内只读工具结果缓存（写执行后失效）。
    tool_cache: bool,
}

/// Type-erased provider that yields the current repo-map text (or `None`)
/// for a given user prompt (personalized seeds, A3).
pub type RepoMapProvider = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Run-start 召回提供器：给定首条用户 prompt，返回可选的"召回上下文"块。
pub type RecallProvider = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Run-end 沉淀钩子：接收本轮组装的 TaskObservation（非阻塞捕获）。
pub type DistillHook = Arc<dyn Fn(TaskObservation) + Send + Sync>;

/// 审查指标计数钩子：name ∈ review_triggered/issues_found/fix_succeeded。
pub type ReviewCounterHook = Arc<dyn Fn(&str) + Send + Sync>;

/// Type-erased closure that inserts a build-time extension value into a
/// ToolContext's `ExtensionRegistry`.
pub(crate) type ExtensionApplier =
    dyn Fn(&mut deepseeknova_core::tool::ExtensionRegistry) + Send + Sync;

/// P2.1 每步 effort 路由所需的双 provider。
#[derive(Clone)]
pub(crate) struct EffortRouting {
    /// 机械续步用：thinking off（省 reasoning token）。
    pub quick: Arc<dyn Provider>,
    /// 首步 / 出错 / 回炉反馈用：高推理。
    pub high: Arc<dyn Provider>,
}

/// P2.2 观察压缩设置。
#[derive(Clone)]
pub(crate) struct ObserveSettings {
    pub provider: Arc<dyn Provider>,
    pub threshold_chars: usize,
    pub max_chars: usize,
}

impl Agent {
    pub fn new(provider: Arc<dyn Provider>, max_steps: usize) -> Self {
        Self {
            provider,
            tools: HashMap::new(),
            max_steps: if max_steps == 0 { 10 } else { max_steps },
            system_prompt: None,
            workspace_root: std::env::current_dir().unwrap_or_default(),
            security: SecurityContext::with_safe_defaults(),

            compaction_threshold_tokens: None,
            history: None,
            permission: None,
            approval: None,
            extensions: Vec::new(),
            repo_map_provider: None,
            recall_provider: None,
            mid_run: None,
            distill_hook: None,
            pause_on_max_steps: true,
            l3_enabled: true,
            compact_provider: None,
            budget: None,
            session_label: None,
            review_provider: None,
            review_settings: None,
            review_counter: None,
            concurrent_tools: true,
            verify_settings: None,
            effort_routing: None,
            observe: None,
            tool_cache: false,
        }
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    /// Append text to the system prompt (used to inject retrieval strategy hints).
    pub fn with_appended_system_prompt(mut self, extra: impl AsRef<str>) -> Self {
        match self.system_prompt {
            Some(ref mut s) => s.push_str(extra.as_ref()),
            None => self.system_prompt = Some(extra.as_ref().to_string()),
        }
        self
    }

    pub fn with_compaction_threshold(mut self, tokens: Option<u32>) -> Self {
        self.compaction_threshold_tokens = tokens;
        self
    }

    /// Attach a persistent conversation store so this agent carries memory
    /// across successive `run_stream` calls. Callers share one
    /// `Arc<Mutex<Vec<Message>>>` across turns (and reset it to start a new
    /// session). When the store is non-empty at run start, the system prompt
    /// is *not* re-injected — the prior turns already contain it.
    pub fn with_conversation_history(
        mut self,
        history: Arc<tokio::sync::Mutex<Vec<Message>>>,
    ) -> Self {
        self.history = Some(history);
        self
    }

    /// Override the workspace root used to confine filesystem tool calls.
    pub fn with_workspace_root(mut self, workspace_root: PathBuf) -> Self {
        self.workspace_root = workspace_root;
        self
    }

    /// Override the security context injected into every tool execution.
    pub fn with_security(mut self, security: SecurityContext) -> Self {
        self.security = security;
        self
    }

    /// Attach a permission gate. When set, tool calls are gated (Allow/Ask/Deny)
    /// before execution.
    pub fn with_permission_gate(mut self, gate: Arc<PermissionGate>) -> Self {
        self.permission = Some(gate);
        self
    }

    /// Attach an approval responder used to resolve `Ask` decisions from the
    /// permission gate.
    pub fn with_approval_responder(mut self, responder: Arc<dyn ApprovalResponder>) -> Self {
        self.approval = Some(responder);
        self
    }

    /// Register an arbitrary extension value that will be injected into the
    /// `ExtensionRegistry` of every tool execution context. Used e.g. to hand
    /// a shared code-graph index to graph tools.
    pub fn with_extension<T: std::any::Any + Send + Sync + Clone>(mut self, ext: T) -> Self {
        self.extensions
            .push(Arc::new(move |reg| reg.insert(ext.clone())));
        self
    }

    /// Attach a repo-map provider closure. At the start of a fresh conversation,
    /// the agent calls this to obtain a code-graph repo map that is appended to
    /// the system prompt (in the stable prefix region, preserving prefix cache
    /// semantics). Returning `None` skips injection.
    pub fn with_repo_map_provider(mut self, provider: RepoMapProvider) -> Self {
        self.repo_map_provider = Some(provider);
        self
    }

    /// 附加起点召回提供器。新会话时以首条 prompt 调用，返回块作为 volatile
    /// User 消息注入（不改动被缓存的 system 前缀）。
    pub fn with_recall_provider(mut self, provider: RecallProvider) -> Self {
        self.recall_provider = Some(provider);
        self
    }

    /// 附加中途检索提供器。续聊且上一轮有工具活动时（或压缩发生后），以
    /// 当前/最近用户消息为查询召回记忆与代码图实体，作为 volatile User 消息
    /// 注入。`require_tool_turn = false` 时每个续聊轮次都注入。
    pub fn with_mid_run_retrieval(
        mut self,
        provider: RecallProvider,
        require_tool_turn: bool,
    ) -> Self {
        self.mid_run = Some(MidRunRetrieval {
            provider,
            require_tool_turn,
        });
        self
    }

    /// 附加结束沉淀钩子。循环结束后组装 TaskObservation 并调用（非阻塞捕获）。
    pub fn with_distill_hook(mut self, hook: DistillHook) -> Self {
        self.distill_hook = Some(hook);
        self
    }

    /// 配置 max_steps 到顶行为："pause"（默认）或 "error"（旧行为逃生舱）。
    pub fn with_on_max_steps(mut self, mode: &str) -> Self {
        self.pause_on_max_steps = mode != "error";
        self
    }

    /// 开关 L3 结构化压缩（false = 仅 L1/L2 现状）。
    pub fn with_l3_compaction(mut self, enabled: bool) -> Self {
        self.l3_enabled = enabled;
        self
    }

    /// 指定 L3 摘要用的（廉价）provider；不设则复用主 provider。
    pub fn with_compact_provider(mut self, p: Arc<dyn Provider>) -> Self {
        self.compact_provider = Some(p);
        self
    }

    /// 启用 step 边界预算守门。
    pub fn with_budget(mut self, b: crate::budget::controller::PromptBudgetController) -> Self {
        self.budget = Some(b);
        self
    }

    /// 标注当前持久化会话 id（Paused 事件透出给前端）。
    pub fn with_session_label(mut self, id: impl Into<String>) -> Self {
        self.session_label = Some(id.into());
        self
    }

    /// 启用完成前自审（B3）。provider 为审查模型，settings 含 diff 上限与轮次。
    pub fn with_review(
        mut self,
        provider: Arc<dyn Provider>,
        diff_cap_tokens: usize,
        max_cycles: usize,
    ) -> Self {
        self.review_provider = Some(provider);
        self.review_settings = Some(crate::review::ReviewSettings {
            diff_cap_tokens,
            max_cycles,
        });
        self
    }

    /// 注入审查指标计数钩子。
    pub fn with_review_counter(mut self, hook: ReviewCounterHook) -> Self {
        self.review_counter = Some(hook);
        self
    }

    /// 控制同批工具调用的执行方式：`true` 时读类工具并发、写类工具保序串行；
    /// `false` 时保持旧的严格串行行为。
    pub fn with_concurrent_tools(mut self, enabled: bool) -> Self {
        self.concurrent_tools = enabled;
        self
    }

    /// 启用完成前确定性验证（P1）：写入轮完成后按 `commands` 经 bash 工具验证，
    /// 失败回炉修复，超过 `max_cycles` 时 Paused(verify_failed)。
    pub fn with_verify(mut self, commands: Vec<String>, max_cycles: usize) -> Self {
        self.verify_settings = Some(crate::verify::VerifySettings {
            commands,
            max_cycles,
        });
        self
    }

    /// P2.1：启用每步 effort 路由。`quick` 用于机械续步（thinking off），
    /// `high` 用于首步 / 出错 / 回炉反馈。
    pub fn with_effort_routing(
        mut self,
        quick: Arc<dyn Provider>,
        high: Arc<dyn Provider>,
    ) -> Self {
        self.effort_routing = Some(EffortRouting { quick, high });
        self
    }

    /// P2.2：启用观察压缩。超 `threshold_chars` 的工具结果由廉价模型摘要为
    /// 至多 `max_chars` 字符后入历史；事件流仍透出原始结果。
    pub fn with_observe_compression(
        mut self,
        provider: Arc<dyn Provider>,
        threshold_chars: usize,
        max_chars: usize,
    ) -> Self {
        self.observe = Some(ObserveSettings {
            provider,
            threshold_chars,
            max_chars,
        });
        self
    }

    /// P2.3：启用会话内只读工具结果缓存（同参读调用直接复用，写执行后失效）。
    pub fn with_tool_cache(mut self, enabled: bool) -> Self {
        self.tool_cache = enabled;
        self
    }

    pub fn register_tool(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.schema().name.clone();
        self.tools.insert(name, tool);
    }

    /// Names of all registered tools (for diagnostics/tests).
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// 诊断：是否装配了中途检索提供器（P3.2，供 runtime 配置生效断言）。
    pub fn mid_run_retrieval_enabled(&self) -> bool {
        self.mid_run.is_some()
    }

    /// Build the ToolContext for a tool call, injecting security + all
    /// build-time registered extensions. Shared by run_stream and tests.
    #[allow(dead_code)] // exercised from tests today; Task 9/10 consume it in-crate
    pub(crate) fn make_tool_context(
        &self,
        call_id: &str,
        cancel: CancellationToken,
    ) -> ToolContext {
        build_tool_context(
            call_id,
            cancel,
            &self.workspace_root,
            &self.security,
            &self.extensions,
        )
    }
}

/// Shared ToolContext construction used by both `Agent::make_tool_context`
/// and the spawned agent loop (which cannot borrow `self`). Keeps the
/// injection set (workspace + security + registered extensions) in one place.
pub(crate) fn build_tool_context(
    call_id: &str,
    cancel: CancellationToken,
    workspace_root: &std::path::Path,
    security: &SecurityContext,
    extensions: &[Arc<ExtensionApplier>],
) -> ToolContext {
    let mut ctx = ToolContext::with_cancellation(call_id, cancel)
        .with_workspace(workspace_root.to_path_buf());
    ctx.extensions.insert(security.clone());
    for apply in extensions {
        apply(&mut ctx.extensions);
    }
    ctx
}

#[async_trait::async_trait]
impl Runner for Agent {
    async fn run_stream(&self, input: RunInput) -> anyhow::Result<RunEventStream> {
        let (tx, rx) = mpsc::channel(64);

        let provider = Arc::clone(&self.provider);
        let tools: Vec<Arc<dyn Tool>> = self.tools.values().cloned().collect();
        let max_steps = self.max_steps;
        let system_prompt = self.system_prompt.clone();
        let compaction_threshold = self.compaction_threshold_tokens;
        let workspace_root = self.workspace_root.clone();
        let security = self.security.clone();
        let history = self.history.clone();
        let permission = self.permission.clone();
        let approval = self.approval.clone();
        let extensions = self.extensions.clone();
        let repo_map_provider = self.repo_map_provider.clone();
        let recall_provider = self.recall_provider.clone();
        let mid_run = self.mid_run.clone();
        let distill_hook = self.distill_hook.clone();
        let pause_on_max_steps = self.pause_on_max_steps;
        let l3_enabled = self.l3_enabled;
        let compact_provider = self.compact_provider.clone();
        // PromptBudgetController 不实现 Clone：从 pub 字段重建一份带进 spawn。
        let budget =
            self.budget
                .as_ref()
                .map(|b| crate::budget::controller::PromptBudgetController {
                    max_total_tokens: b.max_total_tokens,
                    max_memory_tokens: b.max_memory_tokens,
                });
        let session_label = self.session_label.clone();
        let review_provider = self.review_provider.clone();
        // ReviewSettings 不实现 Clone：与 budget 同法，从 pub 字段重建带进 spawn。
        let review_settings =
            self.review_settings
                .as_ref()
                .map(|s| crate::review::ReviewSettings {
                    diff_cap_tokens: s.diff_cap_tokens,
                    max_cycles: s.max_cycles,
                });
        let review_counter = self.review_counter.clone();
        let concurrent_tools = self.concurrent_tools;
        let verify_settings = self.verify_settings.clone();
        let effort_routing = self.effort_routing.clone();
        let observe = self.observe.clone();
        let tool_cache = self.tool_cache;
        let recall_provider_for_loop = recall_provider.clone();

        // Create a cancellation token and wire Ctrl-C (SIGINT) to cancel it.
        // This enables graceful interruption of the agent loop (e.g. Ctrl-C).
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {
                        info!("Ctrl-C received, cancelling agent...");
                        cancel_clone.cancel();
                        break;
                    }
                    _ = cancel_clone.cancelled() => break,
                }
            }
        });

        tokio::spawn(async move {
            let mut memory = Memory::new();

            // Seed working memory from the persistent conversation store, if
            // one is attached. This is what makes the agent remember prior
            // user turns (and preserves DeepSeek-V4 reasoning_content across
            // turns for the must_replay contract).
            let seeded = if let Some(ref hist) = history {
                let prior = hist.lock().await;
                for m in prior.iter() {
                    memory.add_message(m.clone());
                }
                !prior.is_empty()
            } else {
                false
            };

            // Inject the system prompt only on a fresh conversation. When the
            // store already holds prior turns, the system prompt is part of
            // them and re-injecting it would duplicate it.
            if !seeded {
                if let Some(ref sp) = system_prompt {
                    // Build the system prompt content, appending the code-graph
                    // repo map (if any) in the stable prefix region — after the
                    // base prompt, before the volatile conversation — mirroring
                    // context::PromptBuilder's Repo Map format so prefix-cache
                    // semantics hold.
                    // TODO(graph): personalized seeds from user input
                    let mut content = sp.clone();
                    if let Some(ref provider) = repo_map_provider {
                        if let Some(map) = provider(&input.prompt) {
                            if !map.is_empty() {
                                content.push_str("\n\n---\n## Repo Map\n\n```\n");
                                content.push_str(&map);
                                content.push_str("\n```\n");
                            }
                        }
                    }
                    memory.add_message(Message {
                        role: Role::System,
                        content,
                        name: None,
                        tool_calls: None,
                        tool_call_id: None,
                        reasoning_content: None,
                    });
                }
            }

            // Run-start 召回注入（仅新会话）：作为稳定 system 前缀之后的 volatile
            // User 消息插入 —— 保住 DeepSeek-V4 前缀缓存；无 tool_calls/tool_call_id/
            // reasoning，故通过 replay 不变量校验。
            if !seeded {
                if let Some(ref rp) = recall_provider {
                    inject_recall(rp, &mut memory, &input.prompt);
                }
            }

            // P3.3 中途检索：续聊轮次开头，上一轮有工具活动时注入一次
            // 记忆 + 代码图命中（query = 当前用户消息）。
            if seeded {
                if let Some(ref mid) = mid_run {
                    let active =
                        !mid.require_tool_turn || history_last_turn_used_tools(&memory.get_all());
                    if active {
                        inject_recall(&mid.provider, &mut memory, &input.prompt);
                    }
                }
            }

            // 结束沉淀需要的任务文本（input 随后被移入 run_agent_loop）。
            let task_text = input.prompt.clone();
            // F3：记录 run 起始消息数，蒸馏的文件关联只统计本 run 新增的工具调用，
            // 避免续聊会话把历史轮次的文件归到当前任务。
            let run_start_len = memory.get_all().len();

            let result = run_agent_loop(
                provider,
                tools,
                max_steps,
                compaction_threshold,
                &mut memory,
                input,
                &tx,
                &cancel,
                workspace_root,
                security,
                permission,
                approval,
                extensions,
                pause_on_max_steps,
                l3_enabled,
                compact_provider,
                budget,
                session_label,
                review_provider,
                review_settings,
                review_counter,
                concurrent_tools,
                verify_settings,
                effort_routing,
                observe,
                tool_cache,
                recall_provider_for_loop,
                mid_run,
            )
            .await;

            // Persist the full conversation back to the store so the next
            // run_stream call resumes with this context. We write back even
            // on error so partial progress (and any must_replay reasoning) is
            // not silently lost between turns.
            if let Some(ref hist) = history {
                let mut store = hist.lock().await;
                *store = memory.get_all();
            }

            // Run-end 沉淀（非阻塞捕获）：取消时跳过。借用 &result，不影响后续错误日志。
            if let Some(ref hook) = distill_hook {
                if !cancel.is_cancelled() {
                    let msgs = memory.get_all();
                    let tool_calls: Vec<String> = msgs
                        .iter()
                        .filter(|m| m.role == Role::Tool)
                        .filter_map(|m| m.name.clone().or_else(|| m.tool_call_id.clone()))
                        .collect();
                    let steps_taken: Vec<String> = msgs
                        .iter()
                        .filter(|m| m.role == Role::Assistant)
                        .map(|_| "step".to_string())
                        .collect();
                    let (outcome, user_feedback) = match &result {
                        Ok(()) => (TaskOutcome::Success, None),
                        Err(e) => (TaskOutcome::Failure, Some(e.to_string())),
                    };
                    // P3.3 任务-文件关联：从写类工具参数提取触碰文件。
                    let mut seen_files = std::collections::HashSet::new();
                    let files: Vec<String> = msgs
                        .iter()
                        .skip(run_start_len)
                        .filter(|m| m.role == Role::Assistant)
                        .filter_map(|m| m.tool_calls.as_ref())
                        .flatten()
                        .flat_map(|tc| {
                            extract_touched_paths(&tc.function.name, &tc.function.arguments)
                        })
                        .filter(|p| seen_files.insert(p.clone()))
                        .take(20)
                        .collect();
                    hook(TaskObservation {
                        task_description: task_text.clone(),
                        tool_calls,
                        steps_taken,
                        outcome,
                        user_feedback,
                        session_id: "agent".to_string(),
                        files,
                    });
                }
            }

            if let Err(e) = result {
                warn!("agent loop error: {e}");
                let _ = tx.send(Err(e)).await;
            }
        });

        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }
}

// ---------------------------------------------------------------------------
// Agent loop — runs in a spawned task
// ---------------------------------------------------------------------------

/// Accumulated tool call from streaming chunks.
#[derive(Debug, Clone)]
struct PendingToolCall {
    id: String,
    name: String,
    arguments: String,
}

async fn run_agent_loop(
    provider: Arc<dyn Provider>,
    tools: Vec<Arc<dyn Tool>>,
    max_steps: usize,
    compaction_threshold: Option<u32>,
    memory: &mut Memory,
    input: RunInput,
    tx: &mpsc::Sender<anyhow::Result<RunEvent>>,
    cancel: &CancellationToken,
    workspace_root: PathBuf,
    security: SecurityContext,
    permission: Option<Arc<PermissionGate>>,
    approval: Option<Arc<dyn ApprovalResponder>>,
    extensions: Vec<Arc<ExtensionApplier>>,
    pause_on_max_steps: bool,
    l3_enabled: bool,
    compact_provider: Option<Arc<dyn Provider>>,
    budget: Option<crate::budget::controller::PromptBudgetController>,
    session_label: Option<String>,
    review_provider: Option<Arc<dyn Provider>>,
    review_settings: Option<crate::review::ReviewSettings>,
    review_counter: Option<ReviewCounterHook>,
    concurrent_tools: bool,
    verify_settings: Option<crate::verify::VerifySettings>,
    effort_routing: Option<EffortRouting>,
    observe: Option<ObserveSettings>,
    tool_cache: bool,
    recall_provider: Option<RecallProvider>,
    mid_run: Option<MidRunRetrieval>,
) -> anyhow::Result<()> {
    // Add user prompt
    memory.add_message(Message {
        role: Role::User,
        content: input.prompt.clone(),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    });

    // Resource-limit accounting (from SecurityContext.limits). Enforced at
    // step boundaries so each turn stays atomic — preserving the DeepSeek
    // replay invariant (no dangling tool_calls without matching results).
    let run_started = std::time::Instant::now();
    let mut tool_calls_made: usize = 0;

    // B3 审查状态：本轮是否有写类工具执行过 + 已回炉修复的轮次。
    let mut wrote_files = false;
    let mut review_cycles = 0usize;
    // P1 验证状态：写入后确定性验证的失败回炉轮次。
    let mut verify_cycles = 0usize;

    // 会话级 L3 压缩器（持有熔断状态，跨 step 复用）。
    let mut l3 = crate::compaction::L3Compactor::new();
    // P2.3 会话内只读工具结果缓存（写执行后整体失效）。
    let mut tool_cache_map: HashMap<u64, String> = HashMap::new();

    for step in 0..max_steps {
        // Check for cancellation between steps
        if cancel.is_cancelled() {
            tx.send(Ok(RunEvent::Done(RunOutput {
                text: String::new(),
                tool_calls: Vec::new(),
                usage: None,
            })))
            .await
            .ok();
            return Ok(());
        }

        // Resource limits: overall wall-clock deadline and tool-call budget.
        if run_started.elapsed() > security.limits.max_execution_time {
            warn!(
                "agent exceeded max_execution_time ({:?})",
                security.limits.max_execution_time
            );
            return Err(anyhow::anyhow!(
                "exceeded max execution time ({:?})",
                security.limits.max_execution_time
            ));
        }
        if tool_calls_made >= security.limits.max_tool_calls {
            warn!(
                "agent exceeded max_tool_calls ({})",
                security.limits.max_tool_calls
            );
            return Err(anyhow::anyhow!(
                "exceeded max tool calls ({})",
                security.limits.max_tool_calls
            ));
        }

        info!("agent step {}/{}", step + 1, max_steps);

        // B2 预算守门：step 边界评估。CompressHistory 由下方压缩链处理；
        // Reject 时优雅暂停（保留历史写回路径），不再盲目上摊上下文。
        let mut budget_wants_compress = false;
        if let Some(ref b) = budget {
            const EXPECTED_TURN_TOKENS: usize = 2048; // 一轮回复的保守预估
            let current = estimate_tokens(&memory.get_all()) as usize;
            use crate::budget::controller::BudgetDecision;
            match b.evaluate_budget(current, EXPECTED_TURN_TOKENS) {
                BudgetDecision::Allow => {}
                BudgetDecision::CompressHistory => budget_wants_compress = true,
                BudgetDecision::Reject(why) => {
                    warn!("budget rejected further work: {why}");
                    tx.send(Ok(RunEvent::Paused {
                        reason: format!("budget: {why}"),
                        session_id: session_label.clone(),
                    }))
                    .await
                    .ok();
                    return Ok(());
                }
            }
        }

        // Atomic Turn-end compaction
        if compaction_threshold.is_some() || budget_wants_compress {
            let threshold = compaction_threshold.unwrap_or(0);
            let all_msgs = memory.get_all();
            let tokens = estimate_tokens(&all_msgs);

            if tokens > threshold || budget_wants_compress {
                let before = tokens;
                let mut compacted = false;
                // P3.1：传入 token 阈值，shrink 内部按每条消息的 CJK/ASCII
                // 构成换算字符预算，中文场景不再被 4 倍放大。
                memory.shrink_large_results(threshold.max(1));
                let after_shrink = estimate_tokens(&memory.get_all());
                if after_shrink < before {
                    compacted = true;
                }

                info!("shrunk tool results: {} -> {} tokens", before, after_shrink);

                if after_shrink > threshold {
                    warn!("context still over threshold after shrinking tool results. sliding window...");
                    memory.slide_window();
                    compacted = true;
                    let after_slide = estimate_tokens(&memory.get_all());
                    info!("slid window: {} -> {} tokens", after_shrink, after_slide);

                    // B2 L3：L1+L2 仍不够（或 budget 要求压缩）时，结构化摘要。
                    // 已熔断（连败 3 次）则直接跳过，省去渲染开销。
                    if l3_enabled
                        && !l3.is_disabled()
                        && (after_slide > threshold || budget_wants_compress)
                    {
                        let p: &dyn Provider = compact_provider
                            .as_deref()
                            .unwrap_or_else(|| provider.as_ref());
                        if l3.try_compact(p, memory).await {
                            let after_l3 = estimate_tokens(&memory.get_all());
                            info!("L3 compacted: {} -> {} tokens", after_slide, after_l3);
                            compacted = true;
                        }
                    }
                }

                // P3.3 压缩后重建：无论 L1/L2/L3，只要历史发生了驱逐就按
                // 最近用户意图召回注入，避免下一步决策上下文过薄。
                if compacted {
                    let rp = mid_run
                        .as_ref()
                        .map(|m| &m.provider)
                        .or(recall_provider.as_ref());
                    if let Some(rp) = rp {
                        let last_user = crate::compaction::last_user_message(&memory.get_all());
                        if let Some(q) = last_user {
                            inject_recall(rp, memory, &q.content);
                        }
                    }
                }
            }
        }

        // Build the tool index for execution
        let tool_map: HashMap<String, Arc<dyn Tool>> = tools
            .iter()
            .map(|t| (t.schema().name.clone(), Arc::clone(t)))
            .collect();

        // P2.1 每步 provider 选择：机械续步（上一步是正常工具结果）走 quick，
        // 首步 / 出错 / 回炉反馈走 high。
        let step_provider: &Arc<dyn Provider> = if let Some(r) = effort_routing.as_ref() {
            if classify_quick_step(memory) {
                &r.quick
            } else {
                &r.high
            }
        } else {
            &provider
        };

        // Stream from provider
        let step_result = stream_and_process_turn(
            step_provider,
            &tools,
            &tool_map,
            memory,
            tx,
            cancel,
            &workspace_root,
            &security,
            &mut tool_calls_made,
            &mut wrote_files,
            permission.as_ref(),
            approval.as_ref(),
            &extensions,
            concurrent_tools,
            tool_cache,
            &mut tool_cache_map,
            observe.as_ref(),
        )
        .await?;

        match step_result {
            StepOutcome::Complete(output) => {
                // ── P1 完成前确定性验证：有文件写入才触发；bash 缺失或未配置降级放行 ──
                if let Some(vs) = verify_settings.as_ref() {
                    if wrote_files && !vs.commands.is_empty() {
                        match crate::verify::run_verify_pass(
                            &tool_map,
                            vs,
                            &workspace_root,
                            &security,
                            &extensions,
                            cancel,
                            tx,
                        )
                        .await
                        {
                            crate::verify::VerifyOutcome::Pass => {}
                            crate::verify::VerifyOutcome::Fail(reason)
                                if verify_cycles < vs.max_cycles =>
                            {
                                verify_cycles += 1;
                                memory.add_message(Message {
                                    role: Role::User,
                                    content: format!(
                                        "[verification failed]\n{reason}\n\nFix the issues, \
                                         then finish the task. The verification commands will \
                                         run again before completion."
                                    ),
                                    name: None,
                                    tool_calls: None,
                                    tool_call_id: None,
                                    reasoning_content: None,
                                });
                                continue; // 回炉修复，下一次 Complete 再验证
                            }
                            crate::verify::VerifyOutcome::Fail(reason) => {
                                tx.send(Ok(RunEvent::Paused {
                                    reason: format!("verify_failed: {reason}"),
                                    session_id: session_label.clone(),
                                }))
                                .await
                                .ok();
                                return Ok(());
                            }
                            crate::verify::VerifyOutcome::Skipped => {}
                        }
                    }
                }
                // ── B3 完成前自审：有文件写入才触发；降级路径一律放行 Done ──
                if let (Some(rp), Some(rs)) = (&review_provider, &review_settings) {
                    if wrote_files {
                        let bump = |name: &str| {
                            if let Some(ref h) = review_counter {
                                h(name);
                            }
                            info!("review counter: {name}");
                        };
                        match run_review_pass(
                            rp.as_ref(),
                            rs,
                            &workspace_root,
                            &input.prompt,
                            &output.text,
                            &bump,
                            review_cycles == 0,
                        )
                        .await
                        {
                            ReviewOutcome::Approve => {
                                if review_cycles > 0 {
                                    bump("fix_succeeded");
                                }
                            }
                            ReviewOutcome::Issues(issues) if review_cycles < rs.max_cycles => {
                                review_cycles += 1;
                                memory.add_message(Message {
                                    role: Role::User,
                                    content: crate::review::render_feedback(&issues),
                                    name: None,
                                    tool_calls: None,
                                    tool_call_id: None,
                                    reasoning_content: None,
                                });
                                continue; // 回炉修复，下一次 Complete 再审
                            }
                            ReviewOutcome::Issues(issues) => {
                                let head = issues
                                    .iter()
                                    .take(3)
                                    .cloned()
                                    .collect::<Vec<_>>()
                                    .join("; ");
                                tx.send(Ok(RunEvent::Paused {
                                    reason: format!("review_issues: {head}"),
                                    session_id: session_label.clone(),
                                }))
                                .await
                                .ok();
                                return Ok(());
                            }
                            ReviewOutcome::Skipped => {}
                        }
                    }
                }
                tx.send(Ok(RunEvent::Done(output))).await.ok();
                return Ok(());
            }
            StepOutcome::Continue => {
                // Tools were executed; loop continues
                continue;
            }
            StepOutcome::MaxSteps => {
                warn!("agent reached max steps ({max_steps})");
                if pause_on_max_steps {
                    tx.send(Ok(RunEvent::Paused {
                        reason: format!("reached max steps ({max_steps})"),
                        session_id: session_label.clone(),
                    }))
                    .await
                    .ok();
                    return Ok(());
                }
                return Err(anyhow::anyhow!(
                    "reached max steps ({max_steps}) without completing the task"
                ));
            }
        }
    }

    warn!("agent reached max steps ({max_steps})");
    if pause_on_max_steps {
        tx.send(Ok(RunEvent::Paused {
            reason: format!("reached max steps ({max_steps})"),
            session_id: session_label.clone(),
        }))
        .await
        .ok();
        return Ok(());
    }
    Err(anyhow::anyhow!(
        "reached max steps ({max_steps}) without completing the task"
    ))
}

// ---------------------------------------------------------------------------
// B3 pre-completion review
// ---------------------------------------------------------------------------

/// 审查一轮的三态结果。
enum ReviewOutcome {
    Approve,
    Issues(Vec<String>),
    Skipped,
}

/// 执行一次审查：采集 diff → 问审查模型 → 判定。任何失败 → Skipped。
/// `first_pass` 仅用于 review_triggered 只计首轮。
async fn run_review_pass(
    provider: &dyn Provider,
    settings: &crate::review::ReviewSettings,
    workspace_root: &std::path::Path,
    task: &str,
    completion: &str,
    bump: &(dyn Fn(&str) + Send + Sync),
    first_pass: bool,
) -> ReviewOutcome {
    let cap_chars = settings.diff_cap_tokens.saturating_mul(4);
    let Some(diff) = crate::review::collect_diff(workspace_root, cap_chars).await else {
        warn!("review skipped: no git diff available");
        return ReviewOutcome::Skipped;
    };
    if first_pass {
        bump("review_triggered");
    }
    let prompt = crate::review::render_review_prompt(task, completion, &diff);
    match crate::review::ask_reviewer(provider, &prompt).await {
        Some(crate::review::Verdict::Approve) => ReviewOutcome::Approve,
        Some(crate::review::Verdict::Issues(list)) => {
            bump("issues_found");
            ReviewOutcome::Issues(list)
        }
        None => {
            warn!("review skipped: reviewer verdict unavailable/unparseable");
            ReviewOutcome::Skipped
        }
    }
}

// ---------------------------------------------------------------------------
// Turn processing — one provider call + optional tool execution
// ---------------------------------------------------------------------------

enum StepOutcome {
    /// Agent produced final text output — done.
    Complete(RunOutput),
    /// Agent made tool calls — results added to memory, continue loop.
    Continue,
    /// Nothing was produced — max steps will be exhausted.
    MaxSteps,
}

async fn stream_and_process_turn(
    provider: &Arc<dyn Provider>,
    tools: &[Arc<dyn Tool>],
    tool_map: &HashMap<String, Arc<dyn Tool>>,
    memory: &mut Memory,
    tx: &mpsc::Sender<anyhow::Result<RunEvent>>,
    cancel: &CancellationToken,
    workspace_root: &std::path::Path,
    security: &SecurityContext,
    tool_calls_made: &mut usize,
    wrote_files: &mut bool,
    permission: Option<&Arc<PermissionGate>>,
    approval: Option<&Arc<dyn ApprovalResponder>>,
    extensions: &[Arc<ExtensionApplier>],
    concurrent_tools: bool,
    tool_cache_enabled: bool,
    tool_cache: &mut HashMap<u64, String>,
    observe: Option<&ObserveSettings>,
) -> anyhow::Result<StepOutcome> {
    // Build tool refs for provider
    let tool_refs: Vec<&dyn Tool> = tools.iter().map(|t| t.as_ref()).collect();
    let messages = memory.get_all();

    // DeepSeek V4 protocol — ValidatedRequest::new fails early with
    // structured violation list, preventing corrupt messages from
    // ever reaching the provider
    let validated = deepseeknova_provider::ValidatedRequest::new(&messages, &tool_refs).map_err(
        |violations| {
            for v in &violations {
                tracing::error!(?v, "replay invariant violation before provider call");
            }
            anyhow::anyhow!(
                "history replay invariant violated: {} violation(s) detected",
                violations.len()
            )
        },
    )?;

    let mut stream = provider.stream(validated).await?;

    let mut text_buf = String::new();
    let mut reasoning_buf = String::new();
    let mut usage: Option<Usage> = None;
    let mut pending_calls: Vec<PendingToolCall> = Vec::new();

    // Consume the stream
    while let Some(chunk_result) = stream.next().await {
        if cancel.is_cancelled() {
            return Ok(StepOutcome::Complete(RunOutput {
                text: text_buf,
                tool_calls: Vec::new(),
                usage: None,
            }));
        }

        let chunk = chunk_result?;
        match chunk {
            Chunk::TextDelta(delta) => {
                text_buf.push_str(&delta);
                tx.send(Ok(RunEvent::TextDelta(delta))).await.ok();
            }
            Chunk::ReasoningDelta { text, signature } => {
                reasoning_buf.push_str(&text);
                tx.send(Ok(RunEvent::ReasoningDelta { text, signature }))
                    .await
                    .ok();
            }
            Chunk::ToolCallStart { id, name } => {
                // Start accumulating a new tool call
                pending_calls.push(PendingToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: String::new(),
                });
                tx.send(Ok(RunEvent::ToolCallStart { id, name })).await.ok();
            }
            Chunk::ToolCallDelta { id, args_delta } => {
                // Accumulate arguments into the matching pending call
                if let Some(call) = pending_calls.iter_mut().find(|c| c.id == id) {
                    call.arguments.push_str(&args_delta);
                }
                tx.send(Ok(RunEvent::ToolCallDelta { id, args_delta }))
                    .await
                    .ok();
            }
            Chunk::ToolCallEnd {
                id,
                name,
                arguments,
            } => {
                // If we already accumulated from deltas, merge; otherwise use the complete args
                if let Some(call) = pending_calls.iter_mut().find(|c| c.id == id) {
                    if !arguments.is_empty() && call.arguments.is_empty() {
                        call.arguments = arguments.clone();
                    }
                } else {
                    pending_calls.push(PendingToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: arguments.clone(),
                    });
                }
                tx.send(Ok(RunEvent::ToolCallEnd {
                    id,
                    name,
                    arguments,
                }))
                .await
                .ok();
            }
            Chunk::Usage(u) => {
                tx.send(Ok(RunEvent::Usage(u.clone()))).await.ok();
                usage = Some(u);
            }
            Chunk::Done => {}
        }
    }

    // --- Determine what the model wants ---
    let has_text = !text_buf.is_empty();
    let has_tool_calls = !pending_calls.is_empty();

    // Case 1: Only text → final answer
    if has_text && !has_tool_calls {
        // Add assistant message to memory
        memory.add_message(Message {
            role: Role::Assistant,
            content: text_buf.clone(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: if reasoning_buf.is_empty() {
                None
            } else {
                Some(reasoning_buf.clone())
            },
        });

        let final_calls: Vec<ToolCall> = pending_calls
            .into_iter()
            .map(|c| ToolCall {
                id: c.id,
                ty: "function".to_string(),
                function: FunctionCall {
                    name: c.name,
                    arguments: c.arguments,
                },
            })
            .collect();

        return Ok(StepOutcome::Complete(RunOutput {
            text: text_buf,
            tool_calls: final_calls,
            usage,
        }));
    }

    // Case 2: Tool calls (with or without text)
    if has_tool_calls {
        tx.send(Ok(RunEvent::TurnComplete)).await.ok();

        // Add assistant message with tool_calls to memory
        let tool_calls_for_msg: Vec<ToolCall> = pending_calls
            .iter()
            .map(|c| ToolCall {
                id: c.id.clone(),
                ty: "function".to_string(),
                function: FunctionCall {
                    name: c.name.clone(),
                    arguments: c.arguments.clone(),
                },
            })
            .collect();

        memory.add_message(Message {
            role: Role::Assistant,
            content: text_buf.clone(),
            name: None,
            tool_calls: Some(tool_calls_for_msg),
            tool_call_id: None,
            reasoning_content: if reasoning_buf.is_empty() {
                None
            } else {
                Some(reasoning_buf.clone())
            },
        });

        // ── P1 执行调度：权限预检先行，读类并发、写类保序串行 ──
        // 预检按原始顺序串行执行（Ask 等待用户，避免并发弹窗），随后按
        // `read_only` 分段：段内只读工具并发（JoinSet），写工具独占段串行。
        let mut decisions: Vec<Option<String>> = Vec::with_capacity(pending_calls.len());
        for call in &pending_calls {
            let gate_block: Option<String> = match permission {
                Some(gate) => {
                    let decision = match tool_map.get(&call.name) {
                        Some(tool) => gate.check(tool.as_ref(), &call.arguments),
                        None => Decision::Allow,
                    };
                    match decision {
                        Decision::Allow => None,
                        Decision::Deny => Some("blocked by permission policy".to_string()),
                        Decision::Ask => {
                            let approved = if let Some(responder) = approval {
                                let approval_id = format!("approval_{}", uuid::Uuid::new_v4());
                                tx.send(Ok(RunEvent::ApprovalRequest {
                                    id: approval_id.clone(),
                                    title: format!("Allow tool: {}", call.name),
                                    description: Some(call.arguments.clone()),
                                }))
                                .await
                                .ok();
                                // Block until the user answers, but never
                                // deadlock: cancellation resolves to a denial.
                                tokio::select! {
                                    ans = responder.request(
                                        &approval_id,
                                        &call.name,
                                        Some(&call.arguments),
                                    ) => ans,
                                    _ = cancel.cancelled() => false,
                                }
                            } else {
                                // No responder wired (CLI/tests) → allow, so
                                // non-interactive callers keep working.
                                true
                            };
                            if approved {
                                gate.cache_decision(&call.name, &call.arguments, Decision::Allow);
                                None
                            } else {
                                Some("denied by user".to_string())
                            }
                        }
                    }
                }
                None => None,
            };
            decisions.push(gate_block);
        }

        let max_out = security.limits.max_output_bytes as usize;
        let mut results: Vec<Option<String>> = vec![None; pending_calls.len()];
        let mut executed: Vec<bool> = vec![false; pending_calls.len()];
        for (i, call) in pending_calls.iter().enumerate() {
            if let Some(reason) = &decisions[i] {
                results[i] = Some(format!("Error: tool '{}' {reason}", call.name));
            }
        }

        let allowed: Vec<usize> = (0..pending_calls.len())
            .filter(|&i| decisions[i].is_none())
            .collect();
        let segments = group_call_indices(&pending_calls, &allowed, |name| {
            tool_map.get(name).map(|t| t.read_only()).unwrap_or(false)
        });

        for segment in segments {
            if cancel.is_cancelled() {
                break;
            }

            // P2.3 缓存：段内只读调用先查缓存（命中直接落结果，不执行）；
            // 未命中的收集 key，执行后回填；写段执行后整体失效。
            let mut cache_keys: Vec<(usize, u64)> = Vec::new();
            let mut to_execute: Vec<usize> = Vec::new();
            let is_read = |i: usize| {
                tool_map
                    .get(&pending_calls[i].name)
                    .map(|t| t.read_only())
                    .unwrap_or(false)
            };
            for &i in &segment {
                if tool_cache_enabled && is_read(i) {
                    let key = tool_cache_key(&pending_calls[i].name, &pending_calls[i].arguments);
                    if let Some(cached) = tool_cache.get(&key) {
                        results[i] = Some(format!("[cached]\n{cached}"));
                        continue;
                    }
                    cache_keys.push((i, key));
                }
                to_execute.push(i);
            }

            if !to_execute.is_empty() {
                if !concurrent_tools || to_execute.len() <= 1 {
                    for &i in &to_execute {
                        let (idx, result) = execute_tool_call(
                            i,
                            pending_calls[i].clone(),
                            tool_map.clone(),
                            workspace_root.to_path_buf(),
                            security.clone(),
                            extensions.to_vec(),
                            cancel.clone(),
                            max_out,
                        )
                        .await;
                        results[idx] = Some(result);
                        executed[idx] = true;
                    }
                } else {
                    let mut set = JoinSet::new();
                    for &i in &to_execute {
                        let call = pending_calls[i].clone();
                        let tool_map = tool_map.clone();
                        let workspace_root = workspace_root.to_path_buf();
                        let security = security.clone();
                        let extensions = extensions.to_vec();
                        let cancel = cancel.clone();
                        set.spawn(async move {
                            execute_tool_call(
                                i,
                                call,
                                tool_map,
                                workspace_root,
                                security,
                                extensions,
                                cancel,
                                max_out,
                            )
                            .await
                        });
                    }
                    while let Some(joined) = set.join_next().await {
                        if let Ok((idx, result)) = joined {
                            results[idx] = Some(result);
                            executed[idx] = true;
                        }
                    }
                }
            }

            if tool_cache_enabled {
                let mut wrote = false;
                for &i in &to_execute {
                    if !is_read(i) {
                        wrote = true;
                    }
                    if let Some((_, key)) = cache_keys.iter().find(|(idx, _)| *idx == i) {
                        if let Some(r) = &results[i] {
                            if !r.starts_with("Error:") && !r.starts_with("[cached]") {
                                tool_cache.insert(*key, r.clone());
                            }
                        }
                    }
                }
                if wrote {
                    tool_cache.clear();
                }
            }
        }

        // 按原始顺序回写事件与历史（顺序确定，replay 友好）。
        for (i, call) in pending_calls.iter().enumerate() {
            let result = results[i].clone().unwrap_or_else(|| {
                if cancel.is_cancelled() {
                    format!("Error: tool '{}' cancelled before execution", call.name)
                } else {
                    format!("Error: tool '{}' panicked during execution", call.name)
                }
            });
            if executed[i] {
                *tool_calls_made += 1;
                // B3/verify：写类工具或 shell 执行过 → 本轮需验证/审查
                // （名字以注册 schema 为准；shell 工具实际注册名为 "bash"）。
                if matches!(
                    call.name.as_str(),
                    "write_file" | "edit_file" | "move_file" | "bash"
                ) {
                    *wrote_files = true;
                }
            }
            // P2.2 观察压缩：事件透出原始结果；历史存压缩摘要（压缩失败回退原始截断）。
            let stored = if let Some(obs) = observe {
                if result.len() > obs.threshold_chars
                    && !result.starts_with("Error:")
                    && !result.starts_with("[cached]")
                {
                    match compress_observation(obs, &call.name, &result).await {
                        Some(summary) => {
                            format!("[compressed observation for {}]\n{summary}", call.name)
                        }
                        None => result.clone(),
                    }
                } else {
                    result.clone()
                }
            } else {
                result.clone()
            };
            tx.send(Ok(RunEvent::ToolResult {
                call_id: call.id.clone(),
                result: result.clone(),
            }))
            .await
            .ok();
            memory.add_message(Message {
                role: Role::Tool,
                content: stored,
                name: None,
                tool_calls: None,
                tool_call_id: Some(call.id.clone()),
                reasoning_content: None,
            });
        }

        return Ok(StepOutcome::Continue);
    }

    // Case 3: No text, no tool calls — end of stream without meaningful output
    if usage.is_some() {
        // Usage only (some models send a final usage-only chunk after stream ends)
        // This means the model returned nothing — end the turn
        return Ok(StepOutcome::Complete(RunOutput {
            text: String::new(),
            tool_calls: Vec::new(),
            usage,
        }));
    }

    // Nothing produced at all
    warn!("step produced no output");
    Ok(StepOutcome::MaxSteps)
}

// ---------------------------------------------------------------------------
// P1 tool-call scheduling helpers
// ---------------------------------------------------------------------------

/// 将允许执行的下标分组：连续只读调用并入并发段；写类调用独占一段，保序。
/// 未知工具按写（保守）处理，避免并发读写竞争。
fn group_call_indices(
    calls: &[PendingToolCall],
    allowed: &[usize],
    is_read: impl Fn(&str) -> bool,
) -> Vec<Vec<usize>> {
    let mut segments: Vec<Vec<usize>> = Vec::new();
    let mut reads: Vec<usize> = Vec::new();
    for &i in allowed {
        if is_read(&calls[i].name) {
            reads.push(i);
        } else {
            if !reads.is_empty() {
                segments.push(std::mem::take(&mut reads));
            }
            segments.push(vec![i]);
        }
    }
    if !reads.is_empty() {
        segments.push(reads);
    }
    segments
}

/// 执行单个工具调用（并发段内每个任务调用一次），返回 (原始下标, 结果字符串)。
/// 错误与超长输出沿用既有截断策略；本函数不抛错，保证 JoinSet 任务不 panic。
async fn execute_tool_call(
    idx: usize,
    call: PendingToolCall,
    tool_map: HashMap<String, Arc<dyn Tool>>,
    workspace_root: PathBuf,
    security: SecurityContext,
    extensions: Vec<Arc<ExtensionApplier>>,
    cancel: CancellationToken,
    max_out: usize,
) -> (usize, String) {
    let result = if let Some(tool) = tool_map.get(&call.name) {
        info!(tool = %call.name, id = %call.id, "executing tool");
        let ctx = build_tool_context(
            &call.id,
            cancel.child_token(),
            &workspace_root,
            &security,
            &extensions,
        );
        match tool.execute(&ctx, &call.arguments).await {
            Ok(output) => output,
            Err(e) => {
                let err_str = format!("{e:#}");
                // Truncate tool errors to avoid leaking file paths or data into context
                let max_len = 500;
                let truncated = if err_str.len() > max_len {
                    let end = err_str.floor_char_boundary(max_len);
                    format!(
                        "{}... [truncated {} bytes]",
                        &err_str[..end],
                        err_str.len() - end
                    )
                } else {
                    err_str
                };
                format!("Error: {truncated}")
            }
        }
    } else {
        format!("Error: unknown tool '{}'", call.name)
    };

    let result = if result.len() > max_out {
        let end = result.floor_char_boundary(max_out);
        format!(
            "{}... [truncated {} bytes]",
            &result[..end],
            result.len() - end
        )
    } else {
        result
    };
    (idx, result)
}

/// P2.1 每步分类：上一条消息是工具结果且不含 `Error:` → 机械续步（quick）；
/// 其余（首步、出错、回炉反馈）→ high。
fn classify_quick_step(memory: &Memory) -> bool {
    match memory.get_all().last() {
        Some(m) if m.role == Role::Tool => !m.content.contains("Error:"),
        _ => false,
    }
}

/// P3.3 中途检索设置。
#[derive(Clone)]
pub(crate) struct MidRunRetrieval {
    pub(crate) provider: RecallProvider,
    pub(crate) require_tool_turn: bool,
}

/// 上一轮是否有工具活动：从历史末尾向前扫，遇到 Tool 消息 → true；
/// 遇到 User 边界 → false（说明上一轮没有工具调用）。
fn history_last_turn_used_tools(messages: &[Message]) -> bool {
    for m in messages.iter().rev() {
        match m.role {
            Role::Tool => return true,
            Role::User => return false,
            _ => continue,
        }
    }
    false
}

/// 召回注入：把命中块作为 volatile User 消息插入（不触碰 system 前缀，
/// 保住 DeepSeek-V4 前缀缓存）。返回是否实际注入。
fn inject_recall(provider: &RecallProvider, memory: &mut Memory, query: &str) -> bool {
    let Some(block) = provider(query) else {
        return false;
    };
    if block.is_empty() {
        return false;
    }
    memory.add_message(Message {
        role: Role::User,
        content: format!("<recalled-memory>\n{block}\n</recalled-memory>"),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    });
    true
}

/// P2.3 工具缓存 key：(工具名, 参数) 的 SHA-256 前缀 64 位。
fn tool_cache_key(name: &str, args: &str) -> u64 {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(name.as_bytes());
    h.update([0u8]);
    h.update(args.as_bytes());
    let d = h.finalize();
    u64::from_le_bytes([d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]])
}

/// P3.3 从写类工具调用参数提取触碰文件路径（write/edit 用 `path`，
/// move 用 `source`/`destination`）。解析失败返回空。
fn extract_touched_paths(name: &str, args: &str) -> Vec<String> {
    if !matches!(name, "write_file" | "edit_file" | "move_file") {
        return Vec::new();
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(args) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(p) = v.get("path").and_then(|x| x.as_str()) {
        out.push(p.to_string());
    }
    if let Some(s) = v.get("source").and_then(|x| x.as_str()) {
        out.push(s.to_string());
    }
    if let Some(d) = v.get("destination").and_then(|x| x.as_str()) {
        out.push(d.to_string());
    }
    out
}

/// P2.2 观察压缩：用廉价模型把大输出压成结构化摘要；任何失败返回 None（回退截断）。
async fn compress_observation(obs: &ObserveSettings, tool: &str, raw: &str) -> Option<String> {
    let prompt = format!(
        "Compress the following tool output (`{tool}`) into a concise structured \
         summary. Preserve every fact, file path, exit code and number. \
         Output only the summary.\n\n{raw}"
    );
    let msgs = vec![Message {
        role: Role::User,
        content: prompt,
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    }];
    let validated = deepseeknova_provider::ValidatedRequest::new(&msgs, &[]).ok()?;
    let msg = obs.provider.generate(validated).await.ok()?;
    let capped: String = msg.content.chars().take(obs.max_chars).collect();
    if capped.trim().is_empty() {
        None
    } else {
        Some(capped)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::MockProvider;
    use deepseeknova_core::tool::ToolContext;
    use deepseeknova_core::types::ToolSchema;
    use std::sync::Arc;
    use tokio_stream::StreamExt;

    // -----------------------------------------------------------------------
    // Simple structure for testing: a fake tool that records invocations
    // -----------------------------------------------------------------------

    struct SpyTool {
        name: &'static str,
        result: String,
    }

    #[async_trait::async_trait]
    impl Tool for SpyTool {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: self.name.to_string(),
                description: "spy tool".to_string(),
                parameters: serde_json::json!({"type":"object","properties":{}}),
            }
        }
        fn read_only(&self) -> bool {
            true
        }
        async fn execute(&self, _ctx: &ToolContext, _args: &str) -> anyhow::Result<String> {
            Ok(self.result.clone())
        }
    }

    // -----------------------------------------------------------------------
    // Unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn token_estimate_zero_for_empty() {
        assert_eq!(estimate_tokens(&[]), 0);
    }

    #[test]
    fn token_estimate_scales_with_content() {
        let msgs = vec![Message {
            role: Role::User,
            content: "hello world, this is a test message".to_string(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }];
        let tokens = estimate_tokens(&msgs);
        assert!(tokens > 0);
        assert!(tokens < 100);
    }

    // -----------------------------------------------------------------------
    // Integration tests: Agent + MockProvider
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn agent_streams_text_from_provider() {
        let provider = Arc::new(MockProvider::text("hello from agent"));
        let agent = Agent::new(provider, 3);

        let input = RunInput {
            prompt: "say hi".into(),
            images: vec![],
            model_override: None,
        };

        let mut stream = agent.run_stream(input).await.unwrap();
        let mut text = String::new();
        let mut done = false;

        while let Some(event) = stream.next().await {
            match event.unwrap() {
                RunEvent::TextDelta(t) => text.push_str(&t),
                RunEvent::Done(_) => done = true,
                _ => {}
            }
        }

        assert_eq!(text, "hello from agent");
        assert!(done);
    }

    #[tokio::test]
    async fn agent_respects_max_steps() {
        let provider = Arc::new(MockProvider::text("done"));
        let agent = Agent::new(provider, 2);

        let input = RunInput {
            prompt: "do something".into(),
            images: vec![],
            model_override: None,
        };

        let mut stream = agent.run_stream(input).await.unwrap();
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.unwrap());
        }

        assert!(events.iter().any(|e| matches!(e, RunEvent::Done(_))));
    }

    /// 永远只回同一个工具调用的 Agent：每个 step 都 Continue，必然耗尽
    /// max_steps。沿用本模块既有的 MockProvider 单响应（重放）模式。
    fn looping_agent(max_steps: usize) -> Agent {
        let provider = Arc::new(MockProvider::new(vec![
            Chunk::ToolCallStart {
                id: "loop_1".into(),
                name: "spy".into(),
            },
            Chunk::ToolCallEnd {
                id: "loop_1".into(),
                name: "spy".into(),
                arguments: "{}".into(),
            },
            Chunk::Done,
        ]));
        let mut agent = Agent::new(provider, max_steps);
        agent.register_tool(Arc::new(SpyTool {
            name: "spy",
            result: "still going".into(),
        }));
        agent
    }

    #[tokio::test]
    async fn max_steps_pause_emits_paused_not_error() {
        let agent = looping_agent(2)
            .with_on_max_steps("pause")
            .with_session_label("sess-test-1");
        let mut stream = agent
            .run_stream(RunInput {
                prompt: "loop forever".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        let mut saw_paused = false;
        while let Some(ev) = stream.next().await {
            match ev {
                Ok(RunEvent::Paused { reason, session_id }) => {
                    assert!(reason.contains("max steps"));
                    assert_eq!(session_id.as_deref(), Some("sess-test-1"));
                    saw_paused = true;
                }
                Ok(_) => {}
                Err(e) => panic!("pause mode must not surface a stream error: {e}"),
            }
        }
        assert!(saw_paused, "must emit Paused instead of stream error");
    }

    #[tokio::test]
    async fn max_steps_error_mode_keeps_old_behavior() {
        let agent = looping_agent(2).with_on_max_steps("error");
        let mut stream = agent
            .run_stream(RunInput {
                prompt: "loop forever".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        let mut saw_err = false;
        while let Some(ev) = stream.next().await {
            if ev.is_err() {
                saw_err = true;
            }
        }
        assert!(
            saw_err,
            "error mode must surface a stream error (pre-B2 contract)"
        );
    }

    #[tokio::test]
    async fn agent_empty_prompt_still_runs() {
        let provider = Arc::new(MockProvider::text("response to empty"));
        let agent = Agent::new(provider, 3);

        let input = RunInput {
            prompt: "".into(),
            images: vec![],
            model_override: None,
        };

        let mut stream = agent.run_stream(input).await.unwrap();
        let mut text = String::new();
        while let Some(event) = stream.next().await {
            if let Ok(RunEvent::TextDelta(t)) = event {
                text.push_str(&t);
            }
        }

        assert!(text.contains("response to empty"));
    }

    #[tokio::test]
    async fn agent_registers_and_uses_tools() {
        let provider = Arc::new(MockProvider::text("used tool"));
        let mut agent = Agent::new(provider, 3);
        agent.register_tool(Arc::new(SpyTool {
            name: "spy",
            result: "tool ran".into(),
        }));

        let input = RunInput {
            prompt: "use spy".into(),
            images: vec![],
            model_override: None,
        };

        let mut stream = agent.run_stream(input).await.unwrap();
        let mut text = String::new();
        while let Some(event) = stream.next().await {
            if let Ok(RunEvent::TextDelta(t)) = event {
                text.push_str(&t);
            }
        }

        assert!(!text.is_empty(), "agent should produce text output");
    }

    #[tokio::test]
    async fn agent_max_steps_zero_defaults_to_ten() {
        let provider = Arc::new(MockProvider::text("ok"));
        let agent = Agent::new(provider, 0);

        let input = RunInput {
            prompt: "test".into(),
            images: vec![],
            model_override: None,
        };

        let result = agent.run_stream(input).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn agent_system_prompt_injected() {
        let provider = Arc::new(MockProvider::text("got prompt"));
        let agent = Agent::new(provider, 3).with_system_prompt("you are a test bot");

        let input = RunInput {
            prompt: "who are you".into(),
            images: vec![],
            model_override: None,
        };

        let result = agent.run_stream(input).await;
        assert!(
            result.is_ok(),
            "agent with system prompt should run without error"
        );
    }

    #[tokio::test]
    async fn agent_repo_map_provider_injected_into_system_prompt() {
        // A shared persistent store lets us observe the system message that the
        // agent builds at run start (it is persisted verbatim on turn 1).
        let history = Arc::new(tokio::sync::Mutex::new(Vec::<Message>::new()));
        let provider = Arc::new(MockProvider::text("ok"));
        let repo_map: RepoMapProvider =
            Arc::new(|_q| Some("crates/x/src/a.rs:\n│ pub fn foo()\n⋮".to_string()));
        let agent = Agent::new(provider, 3)
            .with_system_prompt("you are a test bot")
            .with_repo_map_provider(repo_map)
            .with_conversation_history(history.clone());

        let mut stream = agent
            .run_stream(RunInput {
                prompt: "who are you".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        let store = history.lock().await;
        let sys = store
            .iter()
            .find(|m| m.role == Role::System)
            .expect("system message should be persisted");
        assert!(sys.content.contains("you are a test bot"));
        assert!(sys.content.contains("Repo Map"));
        assert!(sys.content.contains("pub fn foo()"));
    }

    #[tokio::test]
    async fn agent_repo_map_provider_receives_user_prompt() {
        // A3：repo map 提供器必须收到当前用户输入（个性化 seeds 的基础）。
        let history = Arc::new(tokio::sync::Mutex::new(Vec::<Message>::new()));
        let seen = Arc::new(std::sync::Mutex::new(String::new()));
        let seen_clone = seen.clone();
        let repo_map: RepoMapProvider = Arc::new(move |q: &str| {
            *seen_clone.lock().unwrap() = q.to_string();
            Some("map".to_string())
        });
        let agent = Agent::new(Arc::new(MockProvider::text("ok")), 3)
            .with_system_prompt("sys")
            .with_repo_map_provider(repo_map)
            .with_conversation_history(history.clone());

        let _ = drain(agent, "personalize around CheckpointManager").await;
        let q = seen.lock().unwrap().clone();
        assert!(
            q.contains("CheckpointManager"),
            "repo map provider must receive the user prompt, got: {q}"
        );
    }

    #[tokio::test]
    async fn agent_compaction_threshold_triggers() {
        let provider = Arc::new(MockProvider::text("compacted"));
        let agent = Agent::new(provider, 3).with_compaction_threshold(Some(1));

        let input = RunInput {
            prompt: "a really long message that should trigger compaction".into(),
            images: vec![],
            model_override: None,
        };

        let result = agent.run_stream(input).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn agent_executes_tool_calls() {
        // Mock provider returns a tool call first, then text
        let spy = Arc::new(SpyTool {
            name: "spy",
            result: "tool executed!".into(),
        });
        // Turn 1: tool call -> agent executes it -> continues loop
        // Turn 2: final text -> agent completes
        let responses = vec![
            vec![
                Chunk::ToolCallStart {
                    id: "call_1".into(),
                    name: "spy".into(),
                },
                Chunk::ToolCallEnd {
                    id: "call_1".into(),
                    name: "spy".into(),
                    arguments: "{}".into(),
                },
                Chunk::Done,
            ],
            vec![
                Chunk::TextDelta("done after tool".into()),
                Chunk::Usage(Usage::default()),
                Chunk::Done,
            ],
        ];
        let provider = Arc::new(MockProvider::sequential(responses).with_tools(vec![spy]));
        let mut agent = Agent::new(provider, 5);
        agent.register_tool(Arc::new(SpyTool {
            name: "spy",
            result: "tool executed!".into(),
        }));

        let input = RunInput {
            prompt: "use the tool".into(),
            images: vec![],
            model_override: None,
        };

        let mut stream = agent.run_stream(input).await.unwrap();
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.unwrap());
        }

        // Should see ToolResult and eventually Done
        let has_tool_result = events
            .iter()
            .any(|e| matches!(e, RunEvent::ToolResult { .. }));
        let has_done = events.iter().any(|e| matches!(e, RunEvent::Done(_)));
        assert!(
            has_tool_result,
            "agent should execute tools and emit ToolResult"
        );
        assert!(has_done, "agent should eventually complete with Done");
    }

    #[tokio::test]
    async fn agent_conversation_history_persists_across_runs() {
        // A shared persistent store simulates one desktop/CLI session.
        let history = Arc::new(tokio::sync::Mutex::new(Vec::<Message>::new()));

        // --- Turn 1 ---
        let agent1 = Agent::new(Arc::new(MockProvider::text("first answer")), 3)
            .with_system_prompt("sys")
            .with_conversation_history(history.clone());
        let mut s1 = agent1
            .run_stream(RunInput {
                prompt: "hello".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        // Draining to None guarantees the spawned task finished its writeback
        // (tx is dropped only after the store is persisted).
        while s1.next().await.is_some() {}

        {
            let store = history.lock().await;
            assert!(
                store.iter().any(|m| m.role == Role::System),
                "system prompt should be persisted on the first turn"
            );
            assert!(
                store
                    .iter()
                    .any(|m| m.role == Role::User && m.content == "hello"),
                "first user turn should be persisted"
            );
            assert!(
                store.len() >= 3,
                "expected at least system + user + assistant, got {}",
                store.len()
            );
        }

        // --- Turn 2: a brand-new agent sharing the same history store ---
        let agent2 = Agent::new(Arc::new(MockProvider::text("second answer")), 3)
            .with_system_prompt("sys")
            .with_conversation_history(history.clone());
        let mut s2 = agent2
            .run_stream(RunInput {
                prompt: "again".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while s2.next().await.is_some() {}

        let store = history.lock().await;
        // Both user turns present => memory carried across separate runs.
        assert!(
            store
                .iter()
                .any(|m| m.role == Role::User && m.content == "hello"),
            "turn-1 user message must survive into turn 2"
        );
        assert!(
            store
                .iter()
                .any(|m| m.role == Role::User && m.content == "again"),
            "turn-2 user message must be present"
        );
        // System prompt must NOT be duplicated on the seeded second run.
        let system_count = store.iter().filter(|m| m.role == Role::System).count();
        assert_eq!(
            system_count, 1,
            "system prompt must not be re-injected on a seeded run"
        );
    }

    #[tokio::test]
    async fn agent_persists_reasoning_content_across_turns() {
        // Proves the DeepSeek-V4 adaptation: an assistant turn's
        // reasoning_content is written into the shared history store, so a
        // subsequent run can replay it (must_replay contract spanning turns).
        let history = Arc::new(tokio::sync::Mutex::new(Vec::<Message>::new()));

        let turn1 = vec![
            Chunk::ReasoningDelta {
                text: "let me think about q1".into(),
                signature: Some("sig-1".into()),
            },
            Chunk::TextDelta("the answer".into()),
            Chunk::Usage(Usage::default()),
            Chunk::Done,
        ];
        let agent = Agent::new(Arc::new(MockProvider::new(turn1)), 3)
            .with_conversation_history(history.clone());
        let mut s = agent
            .run_stream(RunInput {
                prompt: "q1".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while s.next().await.is_some() {}

        let store = history.lock().await;
        assert!(
            store.iter().any(|m| m.role == Role::Assistant
                && m.reasoning_content.as_deref() == Some("let me think about q1")),
            "assistant reasoning_content must persist into the shared history \
             so DeepSeek-V4 reasoning replay works across turns"
        );
    }

    // -----------------------------------------------------------------------
    // Permission gate (D3/D4): Deny blocks execution; no gate = unchanged.
    // -----------------------------------------------------------------------

    /// A writer tool that records whether it actually executed.
    struct RecordingTool {
        ran: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait::async_trait]
    impl Tool for RecordingTool {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: "danger".to_string(),
                description: "writer tool".to_string(),
                parameters: serde_json::json!({"type":"object","properties":{}}),
            }
        }
        fn read_only(&self) -> bool {
            false
        }
        async fn execute(&self, _ctx: &ToolContext, _args: &str) -> anyhow::Result<String> {
            self.ran.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok("danger executed".to_string())
        }
    }

    /// Two-turn script: call `danger`, then finish with text.
    fn call_danger_then_done() -> Vec<Vec<Chunk>> {
        vec![
            vec![
                Chunk::ToolCallStart {
                    id: "c1".into(),
                    name: "danger".into(),
                },
                Chunk::ToolCallEnd {
                    id: "c1".into(),
                    name: "danger".into(),
                    arguments: "{}".into(),
                },
                Chunk::Done,
            ],
            vec![
                Chunk::TextDelta("finished".into()),
                Chunk::Usage(Usage::default()),
                Chunk::Done,
            ],
        ]
    }

    #[tokio::test]
    async fn agent_permission_gate_denies_tool() {
        use deepseeknova_permission::{Decision, PermissionGate, Policy, Rule};
        use std::sync::atomic::Ordering;

        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let provider = Arc::new(MockProvider::sequential(call_danger_then_done()));
        let gate = Arc::new(PermissionGate::new(Policy {
            mode: Decision::Ask,
            allow: vec![],
            ask: vec![],
            deny: vec![Rule::new("danger")],
        }));
        let mut agent = Agent::new(provider, 5).with_permission_gate(gate);
        agent.register_tool(Arc::new(RecordingTool { ran: ran.clone() }));

        let mut stream = agent
            .run_stream(RunInput {
                prompt: "go".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        let mut tool_result = String::new();
        while let Some(ev) = stream.next().await {
            if let Ok(RunEvent::ToolResult { result, .. }) = ev {
                tool_result = result;
            }
        }

        assert!(
            !ran.load(Ordering::SeqCst),
            "a Denied tool must NOT execute"
        );
        assert!(
            tool_result.contains("blocked by permission policy"),
            "denied tool result should explain the block, got: {tool_result}"
        );
    }

    #[tokio::test]
    async fn agent_without_gate_executes_tool() {
        use std::sync::atomic::Ordering;

        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let provider = Arc::new(MockProvider::sequential(call_danger_then_done()));
        // No permission gate attached — behavior must be unchanged (tool runs).
        let mut agent = Agent::new(provider, 5);
        agent.register_tool(Arc::new(RecordingTool { ran: ran.clone() }));

        let mut stream = agent
            .run_stream(RunInput {
                prompt: "go".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        assert!(
            ran.load(Ordering::SeqCst),
            "without a gate the tool must execute (behavior unchanged)"
        );
    }

    // -----------------------------------------------------------------------
    // Extension injection hook (Task 8)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn injects_custom_extension_into_tool_context() {
        #[derive(Clone)]
        struct Marker(u32);
        struct ProbeTool;
        #[async_trait::async_trait]
        impl Tool for ProbeTool {
            fn schema(&self) -> ToolSchema {
                ToolSchema {
                    name: "probe".into(),
                    description: "d".into(),
                    parameters: serde_json::json!({"type":"object","properties":{}}),
                }
            }
            fn read_only(&self) -> bool {
                true
            }
            async fn execute(&self, ctx: &ToolContext, _args: &str) -> anyhow::Result<String> {
                let m = ctx.extensions.get::<Marker>().map(|m| m.0).unwrap_or(0);
                Ok(format!("marker={m}"))
            }
        }

        let agent = Agent::new(Arc::new(MockProvider::text("ok")), 3).with_extension(Marker(42));
        let ctx = agent.make_tool_context("call-1", CancellationToken::new());
        let out = ProbeTool.execute(&ctx, "{}").await.unwrap();
        assert_eq!(out, "marker=42");
    }

    #[test]
    fn tool_names_lists_registered() {
        let mut agent = Agent::new(Arc::new(MockProvider::text("ok")), 3);
        agent.register_tool(Arc::new(SpyTool {
            name: "probe2",
            result: "r".into(),
        }));
        assert!(agent.tool_names().iter().any(|n| n == "probe2"));
    }

    #[tokio::test]
    async fn recall_injects_volatile_and_keeps_system_prefix() {
        use std::sync::Mutex as StdMutex;
        struct CapturingProvider {
            seen: Arc<StdMutex<Vec<Message>>>,
        }
        #[async_trait::async_trait]
        impl deepseeknova_provider::Provider for CapturingProvider {
            async fn generate(
                &self,
                _v: deepseeknova_provider::ValidatedRequest<'_>,
            ) -> anyhow::Result<Message> {
                Ok(Message {
                    role: Role::Assistant,
                    content: "done".into(),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                })
            }
            async fn stream(
                &self,
                v: deepseeknova_provider::ValidatedRequest<'_>,
            ) -> anyhow::Result<deepseeknova_core::chunk::ChunkStream> {
                *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = v.messages.to_vec();
                let chunks: Vec<anyhow::Result<deepseeknova_core::chunk::Chunk>> = vec![
                    Ok(deepseeknova_core::chunk::Chunk::TextDelta("done".into())),
                    Ok(deepseeknova_core::chunk::Chunk::Usage(
                        deepseeknova_core::chunk::Usage::default(),
                    )),
                    Ok(deepseeknova_core::chunk::Chunk::Done),
                ];
                Ok(Box::pin(tokio_stream::iter(chunks)))
            }
        }

        let seen = Arc::new(StdMutex::new(Vec::new()));
        let provider = Arc::new(CapturingProvider { seen: seen.clone() });
        let recall: RecallProvider = Arc::new(|_q: &str| Some("REMEMBERED_FACT_XYZ".to_string()));
        let agent = Agent::new(provider, 3)
            .with_system_prompt("SYSTEM_PROMPT_BASE")
            .with_recall_provider(recall);

        let mut stream = agent
            .run_stream(RunInput {
                prompt: "hi".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        let msgs = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(msgs[0].role, Role::System);
        assert!(msgs[0].content.contains("SYSTEM_PROMPT_BASE"));
        assert!(
            !msgs[0].content.contains("REMEMBERED_FACT_XYZ"),
            "recall must NOT be in the cached system prefix"
        );
        assert!(
            msgs.iter()
                .any(|m| m.content.contains("REMEMBERED_FACT_XYZ")),
            "recall must be injected as a volatile message"
        );
    }

    #[tokio::test]
    async fn distill_hook_fires_after_run() {
        use std::sync::Mutex as StdMutex;
        let fired = Arc::new(StdMutex::new(false));
        let f2 = fired.clone();
        let hook: DistillHook = Arc::new(move |_obs| {
            *f2.lock().unwrap_or_else(|e| e.into_inner()) = true;
        });
        let agent = Agent::new(Arc::new(MockProvider::text("ok")), 3)
            .with_system_prompt("sp")
            .with_distill_hook(hook);
        let mut stream = agent
            .run_stream(RunInput {
                prompt: "do it".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}
        // The distill hook fires at the very tail of the spawned task, which may
        // race the stream draining to None. Bounded wait avoids flakiness.
        for _ in 0..50 {
            if *fired.lock().unwrap_or_else(|e| e.into_inner()) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            *fired.lock().unwrap_or_else(|e| e.into_inner()),
            "distill hook should fire"
        );
    }

    // -----------------------------------------------------------------------
    // B3 pre-completion review gate
    // -----------------------------------------------------------------------

    /// 审查模型专用 mock：`generate` 依次弹出队列文本；只剩一个时重复返回
    /// （与 MockProvider 的 stream 语义一致）。主循环不走 stream。
    struct SeqProvider {
        responses: std::sync::Mutex<Vec<String>>,
    }

    impl SeqProvider {
        fn new(responses: Vec<&str>) -> Self {
            Self {
                responses: std::sync::Mutex::new(
                    responses.into_iter().map(str::to_string).collect(),
                ),
            }
        }
    }

    #[async_trait::async_trait]
    impl deepseeknova_provider::Provider for SeqProvider {
        async fn generate(
            &self,
            _v: deepseeknova_provider::ValidatedRequest<'_>,
        ) -> anyhow::Result<Message> {
            let mut lock = self.responses.lock().unwrap_or_else(|e| e.into_inner());
            let content = if lock.len() > 1 {
                lock.remove(0)
            } else {
                lock.first().cloned().unwrap_or_default()
            };
            Ok(Message {
                role: Role::Assistant,
                content,
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            })
        }
        async fn stream(
            &self,
            _v: deepseeknova_provider::ValidatedRequest<'_>,
        ) -> anyhow::Result<deepseeknova_core::chunk::ChunkStream> {
            anyhow::bail!("SeqProvider is generate-only (review path)")
        }
    }

    /// 建一个带未暂存改动的临时 git 仓库（`git diff HEAD` 非空），供审查触发。
    fn temp_git_repo_with_diff(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dnv-b3-review-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q"]);
        std::fs::write(dir.join("f.txt"), "v1\n").unwrap();
        git(&["add", "f.txt"]);
        git(&[
            "-c",
            "user.email=t@test",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "init",
        ]);
        std::fs::write(dir.join("f.txt"), "v2\n").unwrap(); // 未暂存改动
        dir
    }

    /// 主 provider 脚本：先一次写类工具调用，再依次回若干段完成文本。
    fn write_then_texts(texts: &[&str]) -> Vec<Vec<Chunk>> {
        let mut turns = vec![vec![
            Chunk::ToolCallStart {
                id: "w1".into(),
                name: "write_file".into(),
            },
            Chunk::ToolCallEnd {
                id: "w1".into(),
                name: "write_file".into(),
                arguments: "{}".into(),
            },
            Chunk::Done,
        ]];
        for t in texts {
            turns.push(vec![
                Chunk::TextDelta((*t).into()),
                Chunk::Usage(Usage::default()),
                Chunk::Done,
            ]);
        }
        turns
    }

    /// 计数钩子：把每次 bump 的名字收集进共享 Vec。
    fn counting_hook() -> (ReviewCounterHook, Arc<std::sync::Mutex<Vec<String>>>) {
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let s2 = seen.clone();
        let hook: ReviewCounterHook = Arc::new(move |name: &str| {
            s2.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(name.to_string());
        });
        (hook, seen)
    }

    async fn drain(agent: Agent, prompt: &str) -> Vec<RunEvent> {
        let mut stream = agent
            .run_stream(RunInput {
                prompt: prompt.into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            events.push(ev.unwrap());
        }
        events
    }

    #[tokio::test]
    async fn review_disabled_behavior_unchanged() {
        // 不设 with_review：写文件工具跑完后照常 Done，不走审查门。
        let provider = Arc::new(MockProvider::sequential(write_then_texts(&["all done"])));
        let mut agent = Agent::new(provider, 5);
        agent.register_tool(Arc::new(SpyTool {
            name: "write_file",
            result: "written".into(),
        }));

        let events = drain(agent, "write something").await;
        assert!(events.iter().any(|e| matches!(e, RunEvent::Done(_))));
        assert!(!events.iter().any(|e| matches!(e, RunEvent::Paused { .. })));
    }

    #[tokio::test]
    async fn review_issues_then_fix_leads_to_done() {
        let repo = temp_git_repo_with_diff("fix");
        let reviewer = Arc::new(SeqProvider::new(vec![
            r#"{"verdict":"issues","issues":["missing test"]}"#,
            r#"{"verdict":"approve"}"#,
        ]));
        let (hook, seen) = counting_hook();
        // 主 provider：写工具 → 完成声明 v1（被驳回）→ 完成声明 v2（放行）。
        let provider = Arc::new(MockProvider::sequential(write_then_texts(&[
            "done v1", "done v2",
        ])));
        let mut agent = Agent::new(provider, 6)
            .with_workspace_root(repo.clone())
            .with_review(reviewer, 4000, 2)
            .with_review_counter(hook);
        agent.register_tool(Arc::new(SpyTool {
            name: "write_file",
            result: "written".into(),
        }));

        let events = drain(agent, "write something").await;
        assert!(
            events.iter().any(|e| matches!(e, RunEvent::Done(_))),
            "fix cycle must end in Done"
        );
        assert!(!events.iter().any(|e| matches!(e, RunEvent::Paused { .. })));
        let seen = seen.lock().unwrap_or_else(|e| e.into_inner());
        for name in ["review_triggered", "issues_found", "fix_succeeded"] {
            assert_eq!(
                seen.iter().filter(|s| s.as_str() == name).count(),
                1,
                "counter {name} must fire exactly once, got {seen:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[tokio::test]
    async fn review_persistent_issues_pauses() {
        let repo = temp_git_repo_with_diff("pause");
        // 审查 provider 永远回 issues（单响应重复模式）。
        let reviewer = Arc::new(SeqProvider::new(vec![
            r#"{"verdict":"issues","issues":["still broken"]}"#,
        ]));
        let provider = Arc::new(MockProvider::sequential(write_then_texts(&[
            "done v1", "done v2",
        ])));
        let mut agent = Agent::new(provider, 6)
            .with_workspace_root(repo.clone())
            .with_review(reviewer, 4000, 1);
        agent.register_tool(Arc::new(SpyTool {
            name: "write_file",
            result: "written".into(),
        }));

        let events = drain(agent, "write something").await;
        assert!(
            !events.iter().any(|e| matches!(e, RunEvent::Done(_))),
            "persistent issues must NOT reach Done"
        );
        let paused = events.iter().find_map(|e| match e {
            RunEvent::Paused { reason, .. } => Some(reason.clone()),
            _ => None,
        });
        let reason = paused.expect("must emit Paused on persistent review issues");
        assert!(
            reason.starts_with("review_issues"),
            "pause reason must start with review_issues, got: {reason}"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[tokio::test]
    async fn review_skips_outside_git_repo() {
        // 非 git 目录：collect_diff → None → 降级放行 Done，review_triggered 不计。
        let dir = std::env::temp_dir().join(format!(
            "dnv-b3-nogit-agent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let reviewer = Arc::new(SeqProvider::new(vec![r#"{"verdict":"approve"}"#]));
        let (hook, seen) = counting_hook();
        let provider = Arc::new(MockProvider::sequential(write_then_texts(&["all done"])));
        let mut agent = Agent::new(provider, 5)
            .with_workspace_root(dir.clone())
            .with_review(reviewer, 4000, 2)
            .with_review_counter(hook);
        agent.register_tool(Arc::new(SpyTool {
            name: "write_file",
            result: "written".into(),
        }));

        let events = drain(agent, "write something").await;
        assert!(
            events.iter().any(|e| matches!(e, RunEvent::Done(_))),
            "outside a git repo the review must degrade to Done"
        );
        let seen = seen.lock().unwrap_or_else(|e| e.into_inner());
        assert!(
            !seen.iter().any(|s| s == "review_triggered"),
            "skipped review must not count review_triggered, got {seen:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // P1：并行工具执行 + 确定性 Verify
    // -----------------------------------------------------------------------

    /// 写类工具桩（read_only=false → 触发写段串行）。
    struct WritableSpy {
        name: &'static str,
        result: String,
    }

    #[async_trait::async_trait]
    impl Tool for WritableSpy {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: self.name.to_string(),
                description: "writable spy tool".to_string(),
                parameters: serde_json::json!({"type":"object","properties":{}}),
            }
        }
        fn read_only(&self) -> bool {
            false
        }
        async fn execute(&self, _ctx: &ToolContext, _args: &str) -> anyhow::Result<String> {
            Ok(self.result.clone())
        }
    }

    /// bash 工具桩：按 fail 决定验证命令成败。
    struct BashSpy {
        fail: bool,
    }

    #[async_trait::async_trait]
    impl Tool for BashSpy {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: "bash".to_string(),
                description: "bash spy".to_string(),
                parameters: serde_json::json!({"type":"object","properties":{}}),
            }
        }
        fn read_only(&self) -> bool {
            false
        }
        async fn execute(&self, _ctx: &ToolContext, _args: &str) -> anyhow::Result<String> {
            if self.fail {
                anyhow::bail!("command exited with code 1");
            }
            Ok("ok".to_string())
        }
    }

    #[test]
    fn group_call_indices_segments_reads_and_writes_in_order() {
        let calls = vec![
            PendingToolCall {
                id: "a".into(),
                name: "read_file".into(),
                arguments: String::new(),
            },
            PendingToolCall {
                id: "b".into(),
                name: "grep".into(),
                arguments: String::new(),
            },
            PendingToolCall {
                id: "c".into(),
                name: "write_file".into(),
                arguments: String::new(),
            },
            PendingToolCall {
                id: "d".into(),
                name: "read_file".into(),
                arguments: String::new(),
            },
        ];
        let allowed: Vec<usize> = (0..calls.len()).collect();
        let segs = group_call_indices(&calls, &allowed, |n| n != "write_file");
        assert_eq!(segs, vec![vec![0, 1], vec![2], vec![3]]);

        // 全读（或并发关闭）→ 单段，保持原始顺序。
        let segs = group_call_indices(&calls, &allowed, |_| true);
        assert_eq!(segs, vec![vec![0, 1, 2, 3]]);

        // 被权限拦截的下标不参与分段。
        let segs = group_call_indices(&calls, &[1, 3], |n| n != "write_file");
        assert_eq!(segs, vec![vec![1, 3]]);
    }

    #[tokio::test]
    async fn agent_parallel_tool_batch_preserves_result_order() {
        let history = Arc::new(tokio::sync::Mutex::new(Vec::<Message>::new()));
        let provider = Arc::new(MockProvider::sequential(vec![
            vec![
                Chunk::ToolCallStart {
                    id: "call_a".into(),
                    name: "read_file".into(),
                },
                Chunk::ToolCallEnd {
                    id: "call_a".into(),
                    name: "read_file".into(),
                    arguments: "{}".into(),
                },
                Chunk::ToolCallStart {
                    id: "call_b".into(),
                    name: "grep".into(),
                },
                Chunk::ToolCallEnd {
                    id: "call_b".into(),
                    name: "grep".into(),
                    arguments: "{}".into(),
                },
                Chunk::ToolCallStart {
                    id: "call_c".into(),
                    name: "write_file".into(),
                },
                Chunk::ToolCallEnd {
                    id: "call_c".into(),
                    name: "write_file".into(),
                    arguments: "{}".into(),
                },
                Chunk::Done,
            ],
            vec![
                Chunk::TextDelta("done".into()),
                Chunk::Usage(Usage::default()),
                Chunk::Done,
            ],
        ]));
        let mut agent = Agent::new(provider, 5).with_conversation_history(history.clone());
        agent.register_tool(Arc::new(SpyTool {
            name: "read_file",
            result: "R1".into(),
        }));
        agent.register_tool(Arc::new(SpyTool {
            name: "grep",
            result: "R2".into(),
        }));
        agent.register_tool(Arc::new(WritableSpy {
            name: "write_file",
            result: "W3".into(),
        }));

        let events = drain(agent, "use tools").await;
        assert!(
            events.iter().any(|e| matches!(e, RunEvent::Done(_))),
            "batch must finish with Done"
        );
        let results: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                RunEvent::ToolResult { result, .. } => Some(result.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            results,
            vec!["R1", "R2", "W3"],
            "results must keep call order"
        );

        let store = history.lock().await;
        let tool_msgs: Vec<&str> = store
            .iter()
            .filter(|m| m.role == Role::Tool)
            .map(|m| m.content.as_str())
            .collect();
        assert_eq!(tool_msgs, vec!["R1", "R2", "W3"]);
    }

    #[tokio::test]
    async fn verify_gate_retries_then_pauses_on_persistent_failure() {
        let provider = Arc::new(MockProvider::sequential(write_then_texts(&[
            "done v1", "done v2",
        ])));
        let mut agent = Agent::new(provider, 6).with_verify(vec!["cargo check --quiet".into()], 1);
        agent.register_tool(Arc::new(WritableSpy {
            name: "write_file",
            result: "written".into(),
        }));
        agent.register_tool(Arc::new(BashSpy { fail: true }));

        let events = drain(agent, "write something").await;
        assert!(
            events.iter().any(|e| matches!(
                e,
                RunEvent::Paused { reason, .. } if reason.starts_with("verify_failed:")
            )),
            "persistent verify failure must pause, got {events:?}"
        );
        assert!(
            !events.iter().any(|e| matches!(e, RunEvent::Done(_))),
            "persistent verify failure must NOT reach Done"
        );
    }

    #[tokio::test]
    async fn verify_gate_passes_and_reaches_done() {
        let provider = Arc::new(MockProvider::sequential(write_then_texts(&["all done"])));
        let mut agent = Agent::new(provider, 5).with_verify(vec!["cargo check --quiet".into()], 1);
        agent.register_tool(Arc::new(WritableSpy {
            name: "write_file",
            result: "written".into(),
        }));
        agent.register_tool(Arc::new(BashSpy { fail: false }));

        let events = drain(agent, "write something").await;
        assert!(
            events.iter().any(|e| matches!(e, RunEvent::Done(_))),
            "passing verify must reach Done, got {events:?}"
        );
        assert!(!events.iter().any(|e| matches!(e, RunEvent::Paused { .. })));
    }

    /// 只读工具桩：统计执行次数。
    struct CountingSpy {
        name: &'static str,
        result: String,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Tool for CountingSpy {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: self.name.to_string(),
                description: "counting spy".to_string(),
                parameters: serde_json::json!({"type":"object","properties":{}}),
            }
        }
        fn read_only(&self) -> bool {
            true
        }
        async fn execute(&self, _ctx: &ToolContext, _args: &str) -> anyhow::Result<String> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(self.result.clone())
        }
    }

    /// 失败工具桩：执行必错。
    struct FailSpy {
        name: &'static str,
    }

    #[async_trait::async_trait]
    impl Tool for FailSpy {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: self.name.to_string(),
                description: "fail spy".to_string(),
                parameters: serde_json::json!({"type":"object","properties":{}}),
            }
        }
        fn read_only(&self) -> bool {
            true
        }
        async fn execute(&self, _ctx: &ToolContext, _args: &str) -> anyhow::Result<String> {
            anyhow::bail!("boom")
        }
    }

    fn single_read_call(name: &str, id: &str, args: &str) -> Vec<Chunk> {
        vec![
            Chunk::ToolCallStart {
                id: id.into(),
                name: name.into(),
            },
            Chunk::ToolCallEnd {
                id: id.into(),
                name: name.into(),
                arguments: args.into(),
            },
            Chunk::Done,
        ]
    }

    #[tokio::test]
    async fn effort_routing_uses_quick_after_ok_tool_result() {
        let high = Arc::new(MockProvider::sequential(vec![
            single_read_call("read_file", "h1", "{}"),
            vec![
                Chunk::TextDelta("done".into()),
                Chunk::Usage(Usage::default()),
                Chunk::Done,
            ],
        ]));
        let quick = Arc::new(MockProvider::text("quick answer"));
        let mut agent =
            Agent::new(high.clone(), 5).with_effort_routing(quick.clone(), high.clone());
        agent.register_tool(Arc::new(SpyTool {
            name: "read_file",
            result: "ok".into(),
        }));

        let events = drain(agent, "read the file").await;
        assert!(events.iter().any(|e| matches!(e, RunEvent::Done(_))));
        // 首步走 high（1 次），工具结果正常后的续步走 quick（1 次）。
        assert_eq!(high.call_count(), 1, "first step must use high");
        assert_eq!(
            quick.call_count(),
            1,
            "continuation after ok result must use quick"
        );
    }

    #[tokio::test]
    async fn effort_routing_uses_high_after_error_result() {
        let high = Arc::new(MockProvider::sequential(vec![
            single_read_call("read_file", "e1", "{}"),
            vec![
                Chunk::TextDelta("done".into()),
                Chunk::Usage(Usage::default()),
                Chunk::Done,
            ],
        ]));
        let quick = Arc::new(MockProvider::text("quick answer"));
        let mut agent =
            Agent::new(high.clone(), 5).with_effort_routing(quick.clone(), high.clone());
        agent.register_tool(Arc::new(FailSpy { name: "read_file" }));

        let events = drain(agent, "read the file").await;
        assert!(events.iter().any(|e| matches!(e, RunEvent::Done(_))));
        assert_eq!(high.call_count(), 2, "error result must keep using high");
        assert_eq!(
            quick.call_count(),
            0,
            "quick must not be used after an error"
        );
    }

    #[tokio::test]
    async fn observe_compression_stores_summary_keeps_raw_event() {
        let history = Arc::new(tokio::sync::Mutex::new(Vec::<Message>::new()));
        let raw = "x".repeat(20_000);
        let provider = Arc::new(MockProvider::sequential(vec![
            single_read_call("read_file", "o1", "{}"),
            vec![
                Chunk::TextDelta("done".into()),
                Chunk::Usage(Usage::default()),
                Chunk::Done,
            ],
        ]));
        // MockProvider::generate 固定返回 "mock response"，作为压缩摘要。
        let compressor = Arc::new(MockProvider::text("ignored"));
        let mut agent = Agent::new(provider, 5)
            .with_observe_compression(compressor, 1_000, 500)
            .with_conversation_history(history.clone());
        agent.register_tool(Arc::new(SpyTool {
            name: "read_file",
            result: raw.clone(),
        }));

        let events = drain(agent, "read big output").await;
        let raw_event = events
            .iter()
            .find_map(|e| match e {
                RunEvent::ToolResult { result, .. } if result.len() == 20_000 => Some(result),
                _ => None,
            })
            .expect("raw result must be emitted to the event stream");
        assert_eq!(raw_event, &raw);

        let store = history.lock().await;
        let tool_msg = store
            .iter()
            .find(|m| m.role == Role::Tool)
            .expect("tool result must be stored");
        assert!(
            tool_msg
                .content
                .starts_with("[compressed observation for read_file]"),
            "memory must hold the compressed summary, got: {}",
            &tool_msg.content[..60.min(tool_msg.content.len())]
        );
        assert!(tool_msg.content.contains("mock response"));
        assert!(!tool_msg.content.contains(&raw[..1000]));
    }

    #[tokio::test]
    async fn tool_cache_reuses_read_results_across_steps() {
        let history = Arc::new(tokio::sync::Mutex::new(Vec::<Message>::new()));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider = Arc::new(MockProvider::sequential(vec![
            single_read_call("read_file", "c1", r#"{"q":"x"}"#),
            single_read_call("read_file", "c2", r#"{"q":"x"}"#),
            vec![
                Chunk::TextDelta("done".into()),
                Chunk::Usage(Usage::default()),
                Chunk::Done,
            ],
        ]));
        let mut agent = Agent::new(provider, 5)
            .with_tool_cache(true)
            .with_conversation_history(history.clone());
        agent.register_tool(Arc::new(CountingSpy {
            name: "read_file",
            result: "cached payload".into(),
            calls: calls.clone(),
        }));

        let events = drain(agent, "read twice").await;
        assert!(events.iter().any(|e| matches!(e, RunEvent::Done(_))));
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "identical read must hit the session cache"
        );
        let store = history.lock().await;
        let tool_msgs: Vec<&str> = store
            .iter()
            .filter(|m| m.role == Role::Tool)
            .map(|m| m.content.as_str())
            .collect();
        assert_eq!(tool_msgs.len(), 2);
        assert!(tool_msgs[1].starts_with("[cached]"));
    }

    #[tokio::test]
    async fn tool_cache_cleared_after_write() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider = Arc::new(MockProvider::sequential(vec![
            single_read_call("read_file", "d0", r#"{"q":"x"}"#),
            vec![
                Chunk::ToolCallStart {
                    id: "d1".into(),
                    name: "write_file".into(),
                },
                Chunk::ToolCallEnd {
                    id: "d1".into(),
                    name: "write_file".into(),
                    arguments: "{}".into(),
                },
                Chunk::Done,
            ],
            single_read_call("read_file", "d2", r#"{"q":"x"}"#),
            single_read_call("read_file", "d3", r#"{"q":"x"}"#),
            vec![
                Chunk::TextDelta("done".into()),
                Chunk::Usage(Usage::default()),
                Chunk::Done,
            ],
        ]));
        let mut agent = Agent::new(provider, 8).with_tool_cache(true);
        agent.register_tool(Arc::new(CountingSpy {
            name: "read_file",
            result: "r".into(),
            calls: calls.clone(),
        }));
        agent.register_tool(Arc::new(WritableSpy {
            name: "write_file",
            result: "w".into(),
        }));

        let events = drain(agent, "read, write, read again").await;
        assert!(events.iter().any(|e| matches!(e, RunEvent::Done(_))));
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "write must invalidate the cache: read(1) exec, write, read(2) exec, read(3) cached"
        );
    }

    #[test]
    fn inject_recall_adds_volatile_user_message() {
        let mut memory = Memory::new();
        let rp: RecallProvider = Arc::new(|_| Some("hit".to_string()));
        assert!(inject_recall(&rp, &mut memory, "query"));
        let msgs = memory.get_all();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::User);
        assert!(msgs[0].content.contains("hit"));

        let empty: RecallProvider = Arc::new(|_| None);
        assert!(!inject_recall(&empty, &mut memory, "query"));
        assert_eq!(memory.get_all().len(), 1);
    }

    #[tokio::test]
    async fn mid_run_retrieval_injects_on_seeded_tool_turn() {
        // 续聊历史包含一次工具交换 → 新轮次开头触发中途召回注入。
        let history = Arc::new(tokio::sync::Mutex::new(vec![
            Message {
                role: Role::User,
                content: "initial task".into(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            Message {
                role: Role::Assistant,
                content: String::new(),
                name: None,
                tool_calls: Some(vec![ToolCall {
                    id: "x1".into(),
                    ty: "function".to_string(),
                    function: FunctionCall {
                        name: "read_file".into(),
                        arguments: "{}".into(),
                    },
                }]),
                tool_call_id: None,
                reasoning_content: None,
            },
            Message {
                role: Role::Tool,
                content: "ok".into(),
                name: None,
                tool_calls: None,
                tool_call_id: Some("x1".into()),
                reasoning_content: None,
            },
        ]));
        let provider = Arc::new(MockProvider::text("done"));
        let rp: RecallProvider = Arc::new(|_| Some("mid-hit".to_string()));
        let agent = Agent::new(provider, 3)
            .with_conversation_history(history.clone())
            .with_mid_run_retrieval(rp, true);

        let events = drain(agent, "continue the task").await;
        assert!(events.iter().any(|e| matches!(e, RunEvent::Done(_))));
        let store = history.lock().await;
        assert!(
            store.iter().any(|m| m.content.contains("mid-hit")),
            "mid-run retrieval must inject on a seeded tool-active turn"
        );
    }
}
