use crate::diagnose::{DiagnoseGuard, DiagnoseHook};
use crate::memory::Memory;
use crate::prompts::DEFAULT_SYSTEM_PROMPT;
use deepseeknova_core::chunk::{Chunk, Usage};
use deepseeknova_core::memory::skill::{TaskObservation, TaskOutcome};
use deepseeknova_core::protocol::{Phase, PhaseGate, PhaseOutcome, PhaseTransition};
use deepseeknova_core::tool::ToolContext;
use deepseeknova_core::tool_hook::{
    run_user_hook, FindingSeverity, HookEvent, HookPayload, HookVerdict, QualityFinding, ToolHook,
    ToolHookCtx, UserHookCommand, UserHooks,
};
use deepseeknova_core::types::{FunctionCall, ToolCall};
use deepseeknova_core::{
    Message, Role, RunEvent, RunEventStream, RunInput, RunOutput, Runner, Tool,
};
use deepseeknova_metrics::{RunOutcome, SessionSnapshot, SessionTracker};
use deepseeknova_permission::{CheckVerdict, Decision, PermissionGate, RuleSuggestion};
use deepseeknova_provider::auto::AutoRouteDecider;
use deepseeknova_provider::Provider;
use deepseeknova_security::context::SecurityContext;
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
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

    /// 无审批 responder 时的 `Ask` 兜底：`true` = deny（fail-closed，默认），
    /// `false` = allow（旧自动放行契约）。runtime 从
    /// `permissions.ask_without_responder` 装配；裸 `Agent::new` 默认
    /// fail-closed，与子代理侧（sub_agent.rs）的 Ask 语义对齐。
    ask_without_responder_deny: bool,

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
    /// 会话效能快照钩子（run 结束调用一次；None = 关闭）。
    metrics_hook: Option<MetricsHook>,

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

    /// P1/B3 失败回炉前的显式 LLM 反思设置；None = 关闭（默认）。
    reflect_settings: Option<crate::reflection::ReflectSettings>,

    /// 反思教训沉淀钩子（runtime 注入落记忆库；None = 仅对话内）。
    lesson_hook: Option<crate::reflection::LessonHook>,

    /// B 失败归因设置（verify/review 达 max_cycles 的 Paused 前先归因，
    /// reason 附带 fix_plan 摘要）；None = 关闭（默认）。
    attribution_settings: Option<Arc<crate::attribution::AttributionSettings>>,

    /// P2 每步 effort 路由（quick=thinking off / high=高推理）；None = 固定主 provider。
    effort_routing: Option<EffortRouting>,

    /// Auto 模型+思考路由（per-run decider）；None = 关闭。决策在每次
    /// `run_stream` 开始时调用一次，按 run 隔离，不共享跨请求状态。
    auto_router: Option<Arc<dyn AutoRouteDecider>>,

    /// P2 观察压缩设置；None = 关闭（默认）。
    observe: Option<ObserveSettings>,

    /// P2 会话内只读工具结果缓存（写执行后失效）。
    tool_cache: bool,

    /// 工具生命周期钩子（任务质量闭环 A 阶段）：before 预检 / after 策略
    /// 评估，按注册顺序串行执行。
    tool_hooks: Vec<Arc<dyn ToolHook>>,

    /// 用户级外部 hooks（`[hooks]` 配置装配而来）：tool_before 在内部
    /// tool_hook 链之外**额外一层**（内部链 + 用户 hooks 都过才执行），
    /// tool_after / session_start / session_end / failure 为通知型。空 =
    /// 零进程开销。
    user_hooks: UserHooks,

    /// 会话级质量 findings 累计（跨 run 累积；供阶段 B/C 消费，本次只累积）。
    quality_findings: Arc<tokio::sync::Mutex<Vec<QualityFinding>>>,

    /// 失败诊断回调（任务质量闭环 B 阶段）：run 以非 success 结束时构造
    /// [`crate::diagnose::DiagnoseReport`] 传出；None = 关闭（默认）。
    diagnose_hook: Option<DiagnoseHook>,

    /// 协议门控（阶段3）：阶段边界求值的门集合；空 = 协议关闭（零成本路径，
    /// 行为与现状完全一致）。
    protocol_gates: Vec<Arc<dyn PhaseGate>>,

    /// 对抗审查开关（阶段3）：会话收尾按触发条件委派 adversarial-review
    /// 子代理，产出写入诊断报告 `adversarial_review` 字段；默认关闭。
    adversarial_review: bool,
}

/// Type-erased provider that yields the current repo-map text (or `None`)
/// for a given user prompt (personalized seeds, A3).
pub type RepoMapProvider = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Run-start 召回提供器：给定首条用户 prompt，返回可选的"召回上下文"块。
pub type RecallProvider = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Run-end 沉淀钩子：接收本轮组装的 TaskObservation（非阻塞捕获）。
pub type DistillHook = Arc<dyn Fn(TaskObservation) + Send + Sync>;

/// Run-end 效能快照钩子：接收本 run 的 [`SessionSnapshot`] 与 [`QualitySummary`]
/// （非阻塞捕获）。
pub type MetricsHook = Arc<dyn Fn(SessionSnapshot, QualitySummary) + Send + Sync>;

/// Run-end 质量摘要（[`MetricsHook`] 第二参数，任务质量闭环 C）：会话累计的
/// quality findings 快照 + 本 run 的 reflection/review 计数，供评分卡等消费。
/// 定义为 agent 侧私有载体，避免 hook 参数爆炸；runtime 仅消费字段。
#[derive(Debug, Clone, Default)]
pub struct QualitySummary {
    /// 本 run 的会话标注（来自 `with_session_label`；未标注时由 run 生成
    /// 唯一 id）。metrics 落盘与诊断报告共用此 id，保证评分卡/诊断可对账。
    pub session_id: Option<String>,
    /// 本 run 新增的 quality findings（F4 run 级差分切片：run 开始时已存在
    /// 的历史 findings 不计入；跨 run 累计语义由会话级 `Arc\<Mutex\>` 承载）。
    pub findings: Vec<QualityFinding>,
    /// 本 run 失败回炉路径上实际产出 reflection 记录的次数。
    pub reflection_count: u32,
    /// 本 run 审查轮中判定 Approve 的轮数。
    pub review_passes: u32,
    /// 本 run 审查轮中判定 Issues 的轮数。
    pub review_issues: u32,
    /// 协议门控统计（阶段3）：本 run 累计的门控违规数。
    pub protocol_violations: u32,
    /// 协议门控统计（阶段3）：本 run 累计的阶段迁移数。
    pub phase_transitions: u32,
}

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

/// 会话级 quality findings 累积上限（F9：长会话保护）。超过后新 finding
/// 丢弃（仅 warn 一次），避免无界内存增长；事件流照常发出。
pub(crate) const MAX_QUALITY_FINDINGS: usize = 10_000;

/// 会话效能采集守卫：持有本 run 的局部 [`SessionTracker`]，保证每次 run
/// 恰好向 hook 发一次快照——显式终端路径先 `emit(outcome)`，`?` 提前返回
/// 时由 Drop 兜底（outcome=None）。
struct MetricsGuard {
    tracker: SessionTracker,
    hook: Option<MetricsHook>,
    emitted: bool,
    /// 本 run 的会话标注（透传进 QualitySummary，供 metrics 落盘命名）。
    session_label: Option<String>,
    /// 会话累计 findings 的 Arc 引用（emit 时快照进 QualitySummary）。
    quality_findings: Option<Arc<tokio::sync::Mutex<Vec<QualityFinding>>>>,
    /// F4：本 run 起始时会话 findings 长度。emit 时只取 `[start_len..]` 差分
    /// 切片——并发 run 共享同一 Agent 级 `Arc\<Mutex\>` 时，各 run 的 QualitySummary
    /// 只含本 run 新增，不互相污染。`None` = 构造时锁被占用，起始基准未知
    /// （此时 emit 一律报空 findings，绝不回退到 `0` 把并发 run 的 findings
    /// 误切进本 run）。
    start_len: Option<usize>,
    /// 本 run 失败回炉路径上实际产出 reflection 记录的次数。
    reflection_count: u32,
    /// 本 run 审查轮中判定 Approve / Issues 的轮数。
    review_passes: u32,
    review_issues: u32,
    /// 协议门控统计（阶段3）：本 run 累计违规/迁移（emit 时并入
    /// QualitySummary；由 run_agent_loop 每次 transition 后同步）。
    protocol_violations: u32,
    phase_transitions: u32,
    /// 用户级外部 hooks 的 failure 命令列表（run 非成功终点触发；空 = 跳过）。
    failure_hooks: Vec<UserHookCommand>,
    /// 工作区根目录（failure hook 载荷的 `workspace` 字段）。
    workspace_root: PathBuf,
}

impl MetricsGuard {
    fn new(
        hook: Option<MetricsHook>,
        quality_findings: &Arc<tokio::sync::Mutex<Vec<QualityFinding>>>,
        session_label: Option<String>,
        failure_hooks: &[UserHookCommand],
        workspace_root: PathBuf,
    ) -> Self {
        Self {
            tracker: SessionTracker::new(),
            hook,
            emitted: false,
            session_label,
            quality_findings: Some(quality_findings.clone()),
            // F4 起点：try_lock 成功 → 记录真实起始长度；锁忙 → None
            // （"起始基准未知"，emit 时不再回退 0 误切片）。
            start_len: quality_findings.try_lock().ok().map(|g| g.len()),
            reflection_count: 0,
            review_passes: 0,
            review_issues: 0,
            protocol_violations: 0,
            phase_transitions: 0,
            failure_hooks: failure_hooks.to_vec(),
            workspace_root,
        }
    }

    /// 触发用户级外部 hooks 的 failure 事件（失败诊断时）。判定：PausedMaxSteps
    /// （优雅暂停但未完成）或异常返回（outcome=None，Drop 兜底）→ 触发；成功
    /// Completed / 取消 Cancelled（正常终止，不产诊断报告）→ 不触发。失败仅
    /// warn，不阻断。`emit` 是 run 终点的唯一 chokepoint，保证恰好触发一次。
    fn fire_user_failure_hooks(&self, outcome: Option<RunOutcome>) {
        if self.failure_hooks.is_empty() {
            return;
        }
        let is_failure = match outcome {
            Some(RunOutcome::Completed) | Some(RunOutcome::Cancelled) => false,
            Some(RunOutcome::PausedMaxSteps) | None => true,
        };
        if !is_failure {
            return;
        }
        let session_id: &str = self.session_label.as_deref().unwrap_or("unknown");
        let payload = HookPayload {
            event: HookEvent::Failure.as_str(),
            tool: None,
            arguments: None,
            workspace: &self.workspace_root,
            session_id,
        };
        fire_user_notify_hooks(&self.failure_hooks, &payload);
    }

    fn observe_step(&mut self) {
        self.tracker.observe_step();
    }

    fn observe_tool_call(&mut self, name: &str, ok: bool) {
        self.tracker.observe_tool_call(name, ok);
    }

    fn observe_retry(&mut self) {
        self.tracker.observe_retry();
    }

    fn observe_verify(&mut self, passed: bool) {
        self.tracker.observe_verify(passed);
    }

    fn emit(&mut self, outcome: Option<RunOutcome>) {
        if self.emitted {
            return;
        }
        // 用户级外部 hooks：failure 事件在 run 终点触发（失败诊断时）。
        self.fire_user_failure_hooks(outcome);
        if let Some(o) = outcome {
            self.tracker.mark_outcome(o);
        }
        if let Some(ref hook) = self.hook {
            // F4：run 级差分切片——只取本 run 新增的 findings `[start_len..]`。
            // 构造时锁忙（start_len=None）或 emit 时锁忙均报空 findings，
            // 语义为「本次 run 无新增」而非「空数据」；**绝不回退到 `0`**
            // 把并发 run 的 findings 误切进本 run。注意 F9 超限丢弃只发生在
            // start_len 之后，不会把历史 findings 挤进本 run 切片。
            let findings = match self.start_len {
                Some(start) => match self
                    .quality_findings
                    .as_ref()
                    .and_then(|qf| qf.try_lock().ok())
                {
                    Some(guard) => {
                        let all = guard.clone();
                        all.get(start..).unwrap_or(&[]).to_vec()
                    }
                    None => {
                        warn!(
                            "metrics emit: quality_findings lock busy; reporting empty run findings"
                        );
                        Vec::new()
                    }
                },
                None => {
                    warn!(
                        "metrics emit: quality_findings lock busy at run start; \
                         run finding base unknown; reporting empty run findings"
                    );
                    Vec::new()
                }
            };
            let summary = QualitySummary {
                session_id: self.session_label.clone(),
                findings,
                reflection_count: self.reflection_count,
                review_passes: self.review_passes,
                review_issues: self.review_issues,
                protocol_violations: self.protocol_violations,
                phase_transitions: self.phase_transitions,
            };
            hook(self.tracker.snapshot(), summary);
        }
        self.emitted = true;
    }

    /// 记录一次实际产出 reflection 记录的失败回炉路径。
    fn observe_reflection(&mut self) {
        self.reflection_count += 1;
    }

    /// 记录一轮审查判定结果（Approve / Issues）。
    fn observe_review_pass(&mut self) {
        self.review_passes += 1;
    }

    fn observe_review_issues(&mut self) {
        self.review_issues += 1;
    }

    /// 同步协议门控统计（每次 PhaseRunner.transition 后调用）。
    fn sync_protocol_stats(&mut self, violations: u32, transitions: u32) {
        self.protocol_violations = violations;
        self.phase_transitions = transitions;
    }
}

impl Drop for MetricsGuard {
    fn drop(&mut self) {
        self.emit(None);
    }
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
            ask_without_responder_deny: true,
            extensions: Vec::new(),
            repo_map_provider: None,
            recall_provider: None,
            mid_run: None,
            distill_hook: None,
            metrics_hook: None,
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
            reflect_settings: None,
            lesson_hook: None,
            attribution_settings: None,
            effort_routing: None,
            auto_router: None,
            observe: None,
            tool_cache: false,
            tool_hooks: Vec::new(),
            user_hooks: UserHooks::default(),
            quality_findings: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            diagnose_hook: None,
            protocol_gates: Vec::new(),
            adversarial_review: false,
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
            None => {
                self.system_prompt = Some(format!("{DEFAULT_SYSTEM_PROMPT}\n\n{}", extra.as_ref()))
            }
        }
        self
    }

    pub fn with_compaction_threshold(mut self, tokens: Option<u32>) -> Self {
        self.compaction_threshold_tokens = tokens;
        self
    }

    /// Attach a persistent conversation store so this agent carries memory
    /// across successive `run_stream` calls. Callers share one
    /// `Arc\<Mutex\<Vec\<Message\>\>\>` across turns (and reset it to start a new
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

    /// 设置「无审批 responder 时的 `Ask` 兜底」：`true` = deny（fail-closed，
    /// 默认），`false` = allow（旧契约自动放行）。runtime 依据
    /// `permissions.ask_without_responder` 装配；CLI 非交互路径已显式注入
    /// DenyApprovalResponder，此兜底主要覆盖库级/裸 Agent 使用场景。
    pub fn with_ask_without_responder_deny(mut self, deny: bool) -> Self {
        self.ask_without_responder_deny = deny;
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

    /// 附加会话效能快照钩子。每次 `run_stream` 结束恰好调用一次，传入该 run
    /// 的 [`SessionSnapshot`]（含 outcome）与 [`QualitySummary`]（会话 findings
    /// 快照 + reflection/review 计数）；run 隔离由 Agent 保证，跨 run 聚合由
    /// 调用方负责。
    pub fn with_metrics_hook(mut self, hook: MetricsHook) -> Self {
        self.metrics_hook = Some(hook);
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
            llm_provider: None,
            llm_max_chars: 0,
        });
        self
    }

    /// 在确定性验证之上启用 LLM 验证（`[verify] llm = true` 时由 runtime 装配）；
    /// 需先调用 `with_verify`。LLM 明确判定失败才回炉，调用/解析失败优雅跳过。
    pub fn with_llm_verify(mut self, provider: Arc<dyn Provider>, max_chars: usize) -> Self {
        if let Some(vs) = self.verify_settings.as_mut() {
            vs.llm_provider = Some(provider);
            vs.llm_max_chars = max_chars;
        }
        self
    }

    /// 启用失败回炉前的显式 LLM 反思（provider 回落 main 由 runtime 决定）。
    pub fn with_reflection(mut self, provider: Arc<dyn Provider>, max_chars: usize) -> Self {
        self.reflect_settings = Some(crate::reflection::ReflectSettings {
            provider,
            max_chars,
        });
        self
    }

    /// 反思教训沉淀钩子（runtime 注入，落 core 记忆库；None = 仅对话内）。
    pub fn with_lesson_hook(mut self, hook: crate::reflection::LessonHook) -> Self {
        self.lesson_hook = Some(hook);
        self
    }

    /// 注入工具生命周期钩子（任务质量闭环 A 阶段）。可多次调用注册多个
    /// 钩子，before/after 按注册顺序串行执行；钩子 panic 时按 fail-open
    /// 处理（放行 + 空 findings，仅记录 warn）。
    pub fn with_tool_hook(mut self, hook: Arc<dyn ToolHook>) -> Self {
        self.tool_hooks.push(hook);
        self
    }

    /// 挂载用户级外部 hooks（`[hooks]` 配置，runtime 装配）。`tool_before`
    /// 在内部 tool_hook 链之外额外一层（AND 链：内部链 + 用户 hooks 都过
    /// 才执行；任一命令非 0 退出 / 超时 / 崩溃 / 裁决 `allowed=false` →
    /// 阻止执行，fail-closed）；`tool_after` 失败仅 warn；`session_start` /
    /// `session_end` / `failure` 为通知型（失败仅 warn）。空 `UserHooks` =
    /// 零进程开销。
    pub fn with_user_hooks(mut self, hooks: UserHooks) -> Self {
        self.user_hooks = hooks;
        self
    }

    /// 注入协议门控集合（阶段3）：阶段边界对门求值并产出
    /// PhaseTransition/GateViolation/DriftFinding 事件；空集合 = 协议关闭
    /// （零成本路径）。可多次调用追加。门 panic 由调用方按 fail-closed
    /// 处理（与 ToolHook before 语义对齐）。
    pub fn with_protocol_gates(mut self, gates: Vec<Arc<dyn PhaseGate>>) -> Self {
        self.protocol_gates.extend(gates);
        self
    }

    /// 启用对抗审查（阶段3）：会话收尾按触发条件（Blocking finding /
    /// 敏感工具调用）委派 adversarial-review 子代理，产出写入诊断报告
    /// `adversarial_review` 字段；无技能/无 provider 时优雅跳过（warn）。
    /// 默认关闭。
    pub fn with_adversarial_review(mut self, enabled: bool) -> Self {
        self.adversarial_review = enabled;
        self
    }

    /// 注册失败诊断回调（任务质量闭环 B 阶段）。每次 `run_stream` 以非
    /// success 结束（Paused/failed）时调用一次，传入结构化
    /// [`crate::diagnose::DiagnoseReport`]（阶段分解 + 时序 + 失败详情 +
    /// 子代理近似 + 本会话 quality findings）；成功结束不产出。回调 panic
    /// 或报告构造失败不影响主流程（仅记录 warn）。
    pub fn with_diagnose_hook(mut self, hook: DiagnoseHook) -> Self {
        self.diagnose_hook = Some(hook);
        self
    }

    /// 启用失败归因（B）：verify/review 达 max_cycles 的 Paused 前先 LLM
    /// 归因，reason 附带 `fix_plan` 摘要（Paused 恢复时可续用）。归因受
    /// `max_attributions` 硬预算约束（run 内累计，防烧 token），预算超限或
    /// 归因失败时 reason 保持原样（不阻塞、不猜）。
    pub fn with_attribution(mut self, settings: crate::attribution::AttributionSettings) -> Self {
        self.attribution_settings = Some(Arc::new(settings));
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

    /// Auto 模型+思考路由：每次 `run_stream` 开始时调用一次 decider，整轮
    /// 使用其返回的 provider（`None` 回落默认 provider）。决策状态按 run
    /// 隔离，serve 等共享 Agent 的并发请求互不串扰。
    pub fn with_auto_router(mut self, decider: Arc<dyn AutoRouteDecider>) -> Self {
        self.auto_router = Some(decider);
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
        let system_prompt = self
            .system_prompt
            .clone()
            .unwrap_or_else(|| DEFAULT_SYSTEM_PROMPT.to_string());
        let compaction_threshold = self.compaction_threshold_tokens;
        let workspace_root = self.workspace_root.clone();
        let security = self.security.clone();
        let history = self.history.clone();
        let permission = self.permission.clone();
        let approval = self.approval.clone();
        let ask_without_responder_deny = self.ask_without_responder_deny;
        let extensions = self.extensions.clone();
        let repo_map_provider = self.repo_map_provider.clone();
        let recall_provider = self.recall_provider.clone();
        let mid_run = self.mid_run.clone();
        let distill_hook = self.distill_hook.clone();
        let metrics_hook = self.metrics_hook.clone();
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
        let reflect_settings = self.reflect_settings.clone();
        let lesson_hook = self.lesson_hook.clone();
        let attribution_settings = self.attribution_settings.clone();
        let effort_routing = self.effort_routing.clone();
        let auto_router = self.auto_router.clone();
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

        // 任务质量闭环 A：钩子链与会话级 findings 在 spawn 前 clone，
        // 供 spawned task 内引用（避免 self 借用逃逸出方法体）。
        let tool_hooks = self.tool_hooks.clone();
        // 用户级外部 hooks：同法 clone 带进 spawned task（会话边界事件
        // 在 spawned task 内触发；空集合零开销）。
        let user_hooks = self.user_hooks.clone();
        let quality_findings = self.quality_findings.clone();
        // 任务质量闭环 B：诊断回调同法 clone 带进 spawned task。
        let diagnose_hook = self.diagnose_hook.clone();
        // 协议门控（阶段3）：门集合与对抗审查开关 clone 带进 spawned task。
        let protocol_gates = self.protocol_gates.clone();
        let adversarial_review_enabled = self.adversarial_review;

        tokio::spawn(async move {
            // 共享内存：fetch_full_result 工具按需取回被截断的完整结果。
            let memory = Arc::new(tokio::sync::RwLock::new(Memory::new()));

            // Seed working memory from the persistent conversation store, if
            // one is attached. This is what makes the agent remember prior
            // user turns (and preserves DeepSeek-V4 reasoning_content across
            // turns for the must_replay contract).
            let seeded = if let Some(ref hist) = history {
                let prior = hist.lock().await;
                for m in prior.iter() {
                    memory.write().await.add_message(m.clone());
                }
                !prior.is_empty()
            } else {
                false
            };

            // Inject the system prompt only on a fresh conversation. When the
            // store already holds prior turns, the system prompt is part of
            // them and re-injecting it would duplicate it. The default prompt
            // applies whenever the caller did not configure an override.
            if !seeded {
                // Build the system prompt content, appending the code-graph
                // repo map (if any) in the stable prefix region — after the
                // base prompt, before the volatile conversation — mirroring
                // context::PromptBuilder's Repo Map format so prefix-cache
                // semantics hold.
                // TODO(graph): personalized seeds from user input
                let mut content = system_prompt.clone();
                if let Some(ref provider) = repo_map_provider {
                    if let Some(map) = provider(&input.prompt) {
                        if !map.is_empty() {
                            content.push_str("\n\n---\n## Repo Map\n\n```\n");
                            content.push_str(&map);
                            content.push_str("\n```\n");
                        }
                    }
                }
                memory.write().await.add_message(Message {
                    role: Role::System,
                    content,
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                });
            }

            // Run-start 召回注入（仅新会话）：作为稳定 system 前缀之后的 volatile
            // User 消息插入 —— 保住 DeepSeek-V4 前缀缓存；无 tool_calls/tool_call_id/
            // reasoning，故通过 replay 不变量校验。
            if !seeded {
                if let Some(ref rp) = recall_provider {
                    inject_recall(rp, &mut *memory.write().await, &input.prompt);
                }
            }

            // P3.3 中途检索：续聊轮次开头，上一轮有工具活动时注入一次
            // 记忆 + 代码图命中（query = 当前用户消息）。
            if seeded {
                if let Some(ref mid) = mid_run {
                    let active = !mid.require_tool_turn
                        || history_last_turn_used_tools(&memory.read().await.get_all());
                    if active {
                        inject_recall(&mid.provider, &mut *memory.write().await, &input.prompt);
                    }
                }
            }

            // 结束沉淀需要的任务文本（input 随后被移入 run_agent_loop）。
            let task_text = input.prompt.clone();
            // F3：记录 run 起始消息数，蒸馏的文件关联只统计本 run 新增的工具调用，
            // 避免续聊会话把历史轮次的文件归到当前任务。
            let run_start_len = memory.read().await.get_all().len();

            // 注册 fetch_full_result 工具：模型凭 call_id 按需取回被截断的
            // 完整工具结果（与 memory.rs 截断提示配套，消除悬空指令）。
            let mut tools = tools;
            tools.push(Arc::new(crate::fetch_tool::FetchFullResultTool::new(
                memory.clone(),
            )));

            // ── 用户级外部 hooks：session_start（run 启动，一次）──
            // 会话 id 与 run_agent_loop 内部解析一致（显式标注优先，否则
            // 唯一 id）。workspace 克隆供 session_end（workspace_root 随后
            // 移入 run_agent_loop）。
            let session_hook_id = session_label.clone().unwrap_or_else(unique_run_label);
            let session_hook_workspace = workspace_root.clone();
            if !user_hooks.session_start.is_empty() {
                let payload = HookPayload {
                    event: HookEvent::SessionStart.as_str(),
                    tool: None,
                    arguments: None,
                    workspace: &session_hook_workspace,
                    session_id: &session_hook_id,
                };
                fire_user_notify_hooks(&user_hooks.session_start, &payload);
            }

            let result = run_agent_loop(
                provider,
                tools,
                max_steps,
                compaction_threshold,
                memory.clone(),
                input,
                &tx,
                &cancel,
                workspace_root,
                security,
                permission,
                approval,
                ask_without_responder_deny,
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
                reflect_settings,
                lesson_hook,
                attribution_settings,
                effort_routing,
                auto_router,
                observe,
                tool_cache,
                recall_provider_for_loop,
                metrics_hook,
                mid_run,
                &tool_hooks,
                &user_hooks,
                &quality_findings,
                diagnose_hook,
                protocol_gates,
                adversarial_review_enabled,
            )
            .await;

            // ── 用户级外部 hooks：session_end（run 结束，总是触发）──
            // 通知型：失败仅 warn，不阻断。
            if !user_hooks.session_end.is_empty() {
                let payload = HookPayload {
                    event: HookEvent::SessionEnd.as_str(),
                    tool: None,
                    arguments: None,
                    workspace: &session_hook_workspace,
                    session_id: &session_hook_id,
                };
                fire_user_notify_hooks(&user_hooks.session_end, &payload);
            }

            // Persist the full conversation back to the store so the next
            // run_stream call resumes with this context. We write back even
            // on error so partial progress (and any must_replay reasoning) is
            // not silently lost between turns.
            if let Some(ref hist) = history {
                let mut store = hist.lock().await;
                *store = memory.read().await.get_all();
            }

            // Run-end 沉淀（非阻塞捕获）：取消时跳过。借用 &result，不影响后续错误日志。
            if let Some(ref hook) = distill_hook {
                if !cancel.is_cancelled() {
                    let msgs = memory.read().await.get_all();
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
                // 打印完整错误链（{e:?} 展开 source），否则上游 provider 断流时
                // 只能看到最外层 "failed to read chunk from stream"，无法诊断
                // 是超时、连接重置还是 EOF。
                warn!("agent loop error: {e:?}");
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

/// Verify 失败回炉文案（契约：标记 + 原因 + 修复后重跑验证的语义）。
fn verify_failure_message(reason: &str) -> String {
    format!(
        "[verification failed]\n{reason}\n\nFix the issues, then finish the task. \
         The verification commands will run again before completion."
    )
}

// ---------------------------------------------------------------------------
// 协议阶段3：对抗审查（会话收尾触发）
// ---------------------------------------------------------------------------

/// 对抗审查子代理系统提示（内联技能文本；只读审查、不执行工具）。
const ADVERSARIAL_REVIEW_PROMPT: &str = "\
You are an adversarial reviewer for an AI agent session. Review the session \
evidence for security, safety, and permission-boundary issues: secret leakage, \
unsafe commands, sandbox escape attempts, permission policy violations, \
destructive filesystem operations, and unreviewed risky changes.\n\
Respond with a concise findings list. For each finding: severity, location \
(tool call / rule), and a one-line recommendation. If nothing is wrong, say \
so explicitly. Do not invent findings; only report what the evidence supports.";

/// 审查输出预算（字符上限；超出截断）。
const ADVERSARIAL_REVIEW_CAP_CHARS: usize = 4000;
/// 审查输入证据预算（字符上限；超出截断）。
const ADVERSARIAL_REVIEW_INPUT_CAP_CHARS: usize = 6000;
/// 子代理步数预算（防烧 token）。
const ADVERSARIAL_REVIEW_MAX_STEPS: usize = 3;

/// 触发条件判定（纯函数，供测试）：(a) 会话 QualityFinding 存在 Blocking 级；
/// (b) 工具调用命中 security/sandbox/permission 相关路径（工具名或参数）。
pub fn adversarial_review_needed(findings: &[QualityFinding], messages: &[Message]) -> bool {
    if findings
        .iter()
        .any(|f| f.severity == FindingSeverity::Blocking)
    {
        return true;
    }
    messages
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .filter_map(|m| m.tool_calls.as_ref())
        .flatten()
        .any(|tc| tool_call_touches_sensitive_path(&tc.function.name, &tc.function.arguments))
}

/// 敏感工具/参数启发式（Bugbot #9 收紧）：删除/移动类工具无条件敏感
/// （本身高风险）；bash/shell 命令执行类必须同时命中命令内容 marker 才敏感；
/// write/edit 写类必须命中目标路径/内容 marker 才敏感（避免任意 bash 调用、
/// 任意文件写都烧子代理 token）；其余工具参数命中安全边界关键词仍判敏感
/// （如 read_file 读 /etc/passwd）。
fn tool_call_touches_sensitive_path(name: &str, args: &str) -> bool {
    /// 无条件敏感：删除/移动本身高风险。
    const UNCONDITIONALLY_SENSITIVE: [&str; 2] = ["delete_file", "move_file"];
    /// 需叠加 marker 才敏感的工具。
    const MARKER_GATED_TOOLS: [&str; 4] = ["bash", "shell", "write_file", "edit_file"];
    // marker 小写匹配（安全审查 S2）：args 与 marker 都 to_lowercase 后匹配，
    // 防 `Sudo -n`、`Chmod 777`、`/Etc/Passwd` 等大小写变体绕过。
    // 补充常见敏感路径变体：~/.ssh、authorized_keys、crontab、/private/
    // （macOS 上 /etc 为符号链接，真实路径是 /private/etc）、passwd/shadow。
    const SENSITIVE_MARKERS: [&str; 12] = [
        "security",
        "sandbox",
        "permission",
        "sudo",
        "chmod",
        "chown",
        "/etc/",
        "/private/",
        "~/.ssh",
        "authorized_keys",
        "crontab",
        "passwd",
    ];
    if UNCONDITIONALLY_SENSITIVE.contains(&name) {
        return true;
    }
    let lowered = args.to_lowercase();
    let hit = |m: &str| lowered.contains(m);
    if MARKER_GATED_TOOLS.contains(&name) {
        return SENSITIVE_MARKERS.iter().any(|m| hit(m));
    }
    SENSITIVE_MARKERS.iter().any(|m| hit(m))
}

/// 渲染审查输入证据（任务 + findings + 工具调用摘要，字符预算截断）。
fn render_adversarial_evidence(
    task: &str,
    findings: &[QualityFinding],
    messages: &[Message],
) -> String {
    let mut out = format!("# Task\n{task}\n");
    if !findings.is_empty() {
        out.push_str("\n# Quality findings\n");
        for f in findings {
            let sev = match f.severity {
                FindingSeverity::Info => "info",
                FindingSeverity::Warning => "warning",
                FindingSeverity::Blocking => "blocking",
            };
            out.push_str(&format!("- [{sev}] {}: {}\n", f.rule, f.evidence));
        }
    }
    out.push_str("\n# Tool calls\n");
    for m in messages.iter().filter(|m| m.role == Role::Assistant) {
        if let Some(calls) = &m.tool_calls {
            for tc in calls {
                let args: String = tc.function.arguments.chars().take(300).collect();
                out.push_str(&format!("- {}: {args}\n", tc.function.name));
            }
        }
    }
    let cap: String = out
        .chars()
        .take(ADVERSARIAL_REVIEW_INPUT_CAP_CHARS)
        .collect();
    cap
}

/// 会话收尾对抗审查：条件命中时以只读子代理（独立 [`Agent`] 实例，
/// max_steps=3 budget）跑一轮审查并返回产出文本（cap 到输出预算）。
/// 子代理不可用（provider 缺失）/失败/无产出 → `None`（warn 优雅跳过），
/// 不阻断主流程。产出由调用方写入诊断报告 `adversarial_review` 字段。
pub(crate) async fn maybe_spawn_adversarial_review(
    provider: Arc<dyn Provider>,
    task: &str,
    findings: &[QualityFinding],
    messages: &[Message],
    enabled: bool,
) -> Option<String> {
    if !enabled || !adversarial_review_needed(findings, messages) {
        return None;
    }
    let evidence = render_adversarial_evidence(task, findings, messages);
    let prompt = format!("Review the following agent session adversarially.\n\n{evidence}");
    let sub = Agent::new(provider, ADVERSARIAL_REVIEW_MAX_STEPS)
        .with_system_prompt(ADVERSARIAL_REVIEW_PROMPT);
    let input = RunInput {
        prompt,
        images: Vec::new(),
        model_override: None,
    };
    let mut stream = match sub.run_stream(input).await {
        Ok(s) => s,
        Err(e) => {
            warn!("adversarial review skipped (sub-agent unavailable): {e}");
            return None;
        }
    };
    let mut text = String::new();
    while let Some(ev) = stream.next().await {
        match ev {
            Ok(RunEvent::TextDelta(t)) => text.push_str(&t),
            Ok(RunEvent::Done(out)) if !out.text.is_empty() => text = out.text,
            Ok(_) => {}
            Err(e) => {
                warn!("adversarial review skipped (sub-agent failed): {e}");
                return None;
            }
        }
    }
    let capped: String = text.chars().take(ADVERSARIAL_REVIEW_CAP_CHARS).collect();
    if capped.trim().is_empty() {
        warn!("adversarial review skipped (no output)");
        None
    } else {
        Some(capped)
    }
}

/// 会话收尾对抗审查接线（Complete/Paused 共用；Bugbot #2）：启用且条件
/// 命中（Blocking finding / 敏感工具调用，见 [`adversarial_review_needed`]）
/// → 跑只读子代理并注入诊断报告；否则优雅跳过（子代理不可用/无产出 →
/// `None`，不阻断主流程）。Paused 各终端分支（budget / verify / max-steps）
/// 必须在 `diagnose.emit("paused", ..)` **之前**调用——emit 在调用时立即
/// 构建报告，之后注入不生效。
async fn wire_session_adversarial_review(
    diagnose: &mut DiagnoseGuard,
    provider: Arc<dyn Provider>,
    enabled: bool,
    task: &str,
    memory: &Memory,
    quality_findings: &Arc<tokio::sync::Mutex<Vec<QualityFinding>>>,
    start_len: usize,
) {
    if !enabled {
        return;
    }
    // F4：只取本 run 新增的 findings `[start_len..]`——共享 Agent 下并发/
    // 历史 run 的 findings 不得触发或进入本 run 的对抗审查证据。
    let findings: Vec<QualityFinding> = quality_findings
        .lock()
        .await
        .iter()
        .skip(start_len)
        .cloned()
        .collect();
    let msgs = memory.get_all();
    let review = maybe_spawn_adversarial_review(provider, task, &findings, &msgs, true).await;
    diagnose.set_adversarial_review(review);
}

/// 失败回炉前可选反思：无设置/反思失败 → 原文案；成功 → lesson 走钩子、
/// 回炉消息前置反思（根因 + 修复计划）。返回 (回炉文案, 反思产物)——
/// 反思产物供失败诊断报告取 root_cause/fix_plan（任务质量闭环 B 阶段）。
async fn reflect_retry(
    reflect_settings: &Option<crate::reflection::ReflectSettings>,
    lesson_hook: &Option<crate::reflection::LessonHook>,
    task: &str,
    failure: &str,
    completion: &str,
    original: String,
) -> (String, Option<crate::reflection::Reflection>) {
    let Some(settings) = reflect_settings else {
        return (original, None);
    };
    match crate::reflection::run_reflection(
        settings.provider.as_ref(),
        task,
        failure,
        completion,
        settings.max_chars,
    )
    .await
    {
        Some(r) => {
            if let Some(hook) = lesson_hook {
                hook(r.lesson.clone());
            }
            (
                crate::reflection::compose_retry_message(&original, &r),
                Some(r),
            )
        }
        None => (original, None),
    }
}

/// Paused reason 中 fix_plan 摘要的截断上限（字符）。
const PAUSE_FIX_PLAN_CAP: usize = 200;

/// verify/review 达 max_cycles 的 Paused 前归因：预算内调用 LLM，产出
/// `fix_plan` 摘要拼进 reason（恢复时可续用）；预算超限/归因失败 → 原
/// reason（不阻塞、不猜）。verdict 本身不改变 Paused 语义（达上限即暂停）。
async fn attribute_pause_reason(
    attribution: Option<&Arc<crate::attribution::AttributionSettings>>,
    budget: &crate::attribution::AttributionBudget,
    task: &str,
    failure: &str,
    reason: String,
) -> String {
    let Some(cfg) = attribution else {
        return reason;
    };
    if !budget.try_consume() {
        return reason;
    }
    match crate::attribution::run_attribution(
        cfg.provider.as_ref(),
        task,
        failure,
        crate::attribution::MAX_ATTRIBUTION_INPUT_CHARS,
    )
    .await
    {
        Some(a) => match a.fix_plan {
            Some(plan) if !plan.is_empty() => {
                let capped: String = plan.chars().take(PAUSE_FIX_PLAN_CAP).collect();
                format!("{reason}; fix_plan: {capped}")
            }
            _ => reason,
        },
        None => reason,
    }
}

/// 生成 run 级唯一会话标注（`session-<epoch毫秒>-<进程内序号>`，仅含
/// `[A-Za-z0-9_-]`，serve 路径白名单安全）。serve 多会话共享同一 Agent
/// 且未显式标注时，每次 run 必须拿到独立 id，否则 Paused 事件的
/// `session_id` 与诊断报告文件名会互相覆盖。
fn unique_run_label() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!(
        "session-{ms}-{}",
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// 触发一组通知型用户 hooks（session_start / session_end / failure）：
/// 任一命令失败（非 0 退出 / 超时 / 崩溃）仅 warn，不阻断主流程。空列表
/// 零开销（不 spawn 进程）。
fn fire_user_notify_hooks(commands: &[UserHookCommand], payload: &HookPayload) {
    for cmd in commands {
        let run = run_user_hook(cmd, payload);
        if !run.exec.is_allowed() {
            warn!(
                "user hook '{}' ({}) failed: {:?}",
                cmd.command, payload.event, run.exec
            );
        }
    }
}

/// 将权限裁决的"拒绝即教育"建议渲染为人类可读文本；无建议时返回空串。
fn render_suggestions(suggestions: &[RuleSuggestion]) -> String {
    if suggestions.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = suggestions
        .iter()
        .map(|s| {
            let rule = match s.rule.subject {
                Some(ref sub) => format!("{} subject={}", s.rule.tool, sub),
                None => s.rule.tool.clone(),
            };
            format!(
                "[建议] 添加规则即可自动放行: behavior={:?} rule={rule}",
                s.behavior
            )
        })
        .collect();
    lines.join("\n")
}

/// Ask 审批描述的风险前缀（观测台规范：只读 / 非只读 / 危险）。
/// 非 shell 工具或参数不可解析时返回 `None`（保持旧描述不变）。
fn approval_risk_prefix(
    gate: Option<&PermissionGate>,
    tool_name: &str,
    args: &str,
) -> Option<String> {
    let kind = gate?.shell_readonly_kind(tool_name, args)?;
    let label = match kind {
        deepseeknova_security::readonly::ReadOnlyKind::ReadOnly => "只读",
        deepseeknova_security::readonly::ReadOnlyKind::NotReadOnly => "非只读",
        deepseeknova_security::readonly::ReadOnlyKind::Dangerous => "危险",
    };
    Some(format!("[风险:{label}]"))
}

async fn run_agent_loop(
    provider: Arc<dyn Provider>,
    tools: Vec<Arc<dyn Tool>>,
    max_steps: usize,
    compaction_threshold: Option<u32>,
    memory: Arc<tokio::sync::RwLock<Memory>>,
    input: RunInput,
    tx: &mpsc::Sender<anyhow::Result<RunEvent>>,
    cancel: &CancellationToken,
    workspace_root: PathBuf,
    security: SecurityContext,
    permission: Option<Arc<PermissionGate>>,
    approval: Option<Arc<dyn ApprovalResponder>>,
    ask_without_responder_deny: bool,
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
    reflect_settings: Option<crate::reflection::ReflectSettings>,
    lesson_hook: Option<crate::reflection::LessonHook>,
    attribution_settings: Option<Arc<crate::attribution::AttributionSettings>>,
    effort_routing: Option<EffortRouting>,
    auto_router: Option<Arc<dyn AutoRouteDecider>>,
    observe: Option<ObserveSettings>,
    tool_cache: bool,
    recall_provider: Option<RecallProvider>,
    metrics_hook: Option<MetricsHook>,
    mid_run: Option<MidRunRetrieval>,
    tool_hooks: &[Arc<dyn ToolHook>],
    user_hooks: &UserHooks,
    quality_findings: &Arc<tokio::sync::Mutex<Vec<QualityFinding>>>,
    diagnose_hook: Option<DiagnoseHook>,
    protocol_gates: Vec<Arc<dyn PhaseGate>>,
    adversarial_review_enabled: bool,
) -> anyhow::Result<()> {
    // serve 等共享 Agent 场景未配置会话标注：每次 run 生成唯一 id，避免
    // 多会话共用同一 Paused session_id / 诊断报告文件名互相覆盖。
    let session_label = session_label.or_else(|| Some(unique_run_label()));
    // 会话 id（用户 hooks JSON 载荷的 `session_id` 字段）。
    let session_id: &str = session_label.as_deref().unwrap_or("unknown");

    // Add user prompt
    memory.write().await.add_message(Message {
        role: Role::User,
        content: input.prompt.clone(),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    });

    // Auto 模型+思考路由：每 run 决策一次（而非每步），决策状态随 run 隔离，
    // serve 等共享 Agent 的并发请求互不串扰；显式 model_override 或决策失败/
    // 无匹配指针时回落默认 provider。
    let auto_provider = if let Some(ref decider) = auto_router {
        if input.model_override.is_none() {
            decider.decide(&memory.read().await.get_all()).await
        } else {
            None
        }
    } else {
        None
    };

    // Resource-limit accounting (from SecurityContext.limits). Enforced at
    // step boundaries so each turn stays atomic — preserving the DeepSeek
    // replay invariant (no dangling tool_calls without matching results).
    let run_started = std::time::Instant::now();
    let mut tool_calls_made: usize = 0;

    // B3 审查状态：本轮是否有写类工具执行过 + 已回炉修复的轮次。
    let mut wrote_files = false;
    let mut review_cycles = 0usize;
    // 任务质量闭环 A：本轮是否产出过 Blocking finding（B3 review 短路前置）。
    let mut quality_blocked = false;
    // P1 验证状态：写入后确定性验证的失败回炉轮次。
    let mut verify_cycles = 0usize;

    // 会话级 L3 压缩器（持有熔断状态，跨 step 复用）。
    let mut l3 = crate::compaction::L3Compactor::new();
    // P2.3 会话内只读工具结果缓存（写执行后整体失效）。
    let mut tool_cache_map: HashMap<u64, String> = HashMap::new();
    // 会话效能采集（局部 tracker，run 隔离）。
    // 用户 hooks 的 failure 事件在 MetricsGuard::emit 触发（run 终点的唯一
    // chokepoint，可精确区分 PausedMaxSteps/异常返回与成功/取消）。
    let mut metrics = MetricsGuard::new(
        metrics_hook,
        quality_findings,
        session_label.clone(),
        &user_hooks.failure,
        workspace_root.clone(),
    );
    // F4：本 run 起始时会话 findings 长度。DiagnoseGuard 与对抗审查同样
    // 按此起点切片，保证诊断报告/审查只含本 run 新增，不被并发 run 或
    // 历史 run 的 findings 污染（MetricsGuard 内部也按同一时刻切片）。
    let quality_start_len = quality_findings.lock().await.len();
    // 任务质量闭环 B：失败诊断采集（Paused/failed 结束路径产出报告；
    // 成功路径 suppress 关闭；Drop 兜底异常路径）。
    let mut diagnose = DiagnoseGuard::new(
        diagnose_hook,
        session_label.clone(),
        Arc::clone(quality_findings),
        quality_start_len,
    );
    diagnose.phase_enter("plan");
    // 协议门控（阶段3）：会话级阶段运行器；门集合为空时走零成本路径。
    let mut phase_runner = crate::phase_runner::PhaseRunner::new();
    let protocol_active = !protocol_gates.is_empty();
    // B 失败归因预算（run 内累计，防烧 token；未配置归因时上限 0 = 关闭）。
    let attribution_budget = crate::attribution::AttributionBudget::new(
        attribution_settings
            .as_ref()
            .map(|s| s.max_attributions)
            .unwrap_or(0),
    );

    for step in 0..max_steps {
        metrics.observe_step();
        // Check for cancellation between steps
        if cancel.is_cancelled() {
            metrics.emit(Some(RunOutcome::Cancelled));
            // F5：取消是正常终止（metrics 已计 Cancelled），不产诊断报告；
            // suppress 防止 Drop 兜底误报 outcome=failed。
            diagnose.suppress();
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

        // A1 热路径：每步取一次会话历史快照，步内 budget 判定 / 压缩判定 /
        // 机械续步分类 / provider 请求复用同一快照，消除步内 5-10 次全量
        // 克隆。压缩等**修改会话**的路径在修改后重新快照（provider 必须
        // 看到压缩后的历史）；其余步内对会话的写入在下一步反映，语义与
        // 既有实现一致（原实现本就在 provider 请求处取同一时刻的快照）。
        let mut snapshot = memory.read().await.get_all();

        // B2 预算守门：step 边界评估。CompressHistory 由下方压缩链处理；
        // Reject 时优雅暂停（保留历史写回路径），不再盲目上摊上下文。
        let mut budget_wants_compress = false;
        if let Some(ref b) = budget {
            const EXPECTED_TURN_TOKENS: usize = 2048; // 一轮回复的保守预估
            let current = estimate_tokens(&snapshot) as usize;
            use crate::budget::controller::BudgetDecision;
            match b.evaluate_budget(current, EXPECTED_TURN_TOKENS) {
                BudgetDecision::Allow => {}
                BudgetDecision::CompressHistory => budget_wants_compress = true,
                BudgetDecision::Reject(why) => {
                    warn!("budget rejected further work: {why}");
                    metrics.emit(None);
                    diagnose.record_failure(
                        "budget",
                        None,
                        None,
                        format!("budget rejected: {why}"),
                    );
                    // Bugbot #2：Paused 终端分支同样接对抗审查（spec §4.2
                    // 无 unverified 限定）；须在 emit 之前注入。
                    wire_session_adversarial_review(
                        &mut diagnose,
                        provider.clone(),
                        adversarial_review_enabled,
                        &input.prompt,
                        &*memory.read().await,
                        quality_findings,
                        quality_start_len,
                    )
                    .await;
                    diagnose
                        .emit("paused", &memory.read().await.get_all())
                        .await;
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
            // A1：判定基于步内快照（与 budget 判定同源，零额外克隆）。
            let tokens = estimate_tokens(&snapshot);

            if tokens > threshold || budget_wants_compress {
                let before = tokens;
                let mut compacted = false;
                // P3.1：传入 token 阈值，shrink 内部按每条消息的 CJK/ASCII
                // 构成换算字符预算，中文场景不再被 4 倍放大。
                memory.write().await.shrink_large_results(threshold.max(1));
                // A1：中间 token 估算用零拷贝接口（`&self` 只读借用），
                // 不为此全量克隆历史。
                let after_shrink = memory.read().await.estimate_tokens();
                if after_shrink < before {
                    compacted = true;
                }

                info!("shrunk tool results: {} -> {} tokens", before, after_shrink);

                if after_shrink > threshold {
                    warn!("context still over threshold after shrinking tool results. sliding window...");
                    memory.write().await.slide_window();
                    compacted = true;
                    let after_slide = memory.read().await.estimate_tokens();
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
                        if l3.try_compact(p, &mut *memory.write().await).await {
                            let after_l3 = memory.read().await.estimate_tokens();
                            info!("L3 compacted: {} -> {} tokens", after_slide, after_l3);
                            compacted = true;
                        }
                    }
                }

                // P3.3 压缩后重建：无论 L1/L2/L3，只要历史发生了驱逐就按
                // 最近用户意图召回注入，避免下一步决策上下文过薄。
                if compacted {
                    // A1：压缩已修改历史 → 在此统一重新快照（last_user 重建
                    // 与后续 provider 请求都需要压缩后的最新历史）。
                    snapshot = memory.read().await.get_all();
                    let rp = mid_run
                        .as_ref()
                        .map(|m| &m.provider)
                        .or(recall_provider.as_ref());
                    if let Some(rp) = rp {
                        let last_user = crate::compaction::last_user_message(&snapshot);
                        if let Some(q) = last_user {
                            inject_recall(rp, &mut *memory.write().await, &q.content);
                            // 召回注入修改了历史 → 再次快照。
                            snapshot = memory.read().await.get_all();
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

        // P2.1 每步 provider 选择：auto 路由（整轮固定）优先；否则机械续步
        // （上一步是正常工具结果）走 quick，首步 / 出错 / 回炉反馈走 high。
        let step_provider: &Arc<dyn Provider> = if let Some(p) = auto_provider.as_ref() {
            p
        } else if let Some(r) = effort_routing.as_ref() {
            if classify_quick_step(&snapshot) {
                &r.quick
            } else {
                &r.high
            }
        } else {
            &provider
        };

        // ── 协议门控（阶段3）：阶段边界。Understand 仅首轮；Plan 每轮
        // LLM 调用前推进。违规进事件流；stats 同步给 MetricsGuard。
        if protocol_active {
            if step == 0 {
                // Bugbot #12：findings 接会话质量容器实时快照（首轮通常为空，
                // 但自定义门可依赖 ctx.findings 的非空语义）。
                let findings = quality_findings.lock().await.clone();
                let ctx =
                    phase_runner.build_ctx(Phase::Understand, verify_settings.is_some(), findings);
                let violations = phase_runner.transition(Phase::Understand, &protocol_gates, &ctx);
                tx.send(Ok(RunEvent::PhaseTransition {
                    transition: PhaseTransition {
                        phase: Phase::Understand,
                        outcome: if violations.is_empty() {
                            PhaseOutcome::Pass
                        } else {
                            PhaseOutcome::Violated
                        },
                    },
                }))
                .await
                .ok();
                for v in &violations {
                    tx.send(Ok(RunEvent::GateViolation(v.clone()))).await.ok();
                }
                let (pv, pt) = phase_runner.stats();
                metrics.sync_protocol_stats(pv, pt);
            }
            let findings = quality_findings.lock().await.clone();
            let ctx = phase_runner.build_ctx(Phase::Plan, verify_settings.is_some(), findings);
            let violations = phase_runner.transition(Phase::Plan, &protocol_gates, &ctx);
            tx.send(Ok(RunEvent::PhaseTransition {
                transition: PhaseTransition {
                    phase: Phase::Plan,
                    outcome: if violations.is_empty() {
                        PhaseOutcome::Pass
                    } else {
                        PhaseOutcome::Violated
                    },
                },
            }))
            .await
            .ok();
            for v in &violations {
                tx.send(Ok(RunEvent::GateViolation(v.clone()))).await.ok();
            }
            let (pv, pt) = phase_runner.stats();
            metrics.sync_protocol_stats(pv, pt);
        }

        // Stream from provider
        let step_result = stream_and_process_turn(
            step_provider,
            &tools,
            &tool_map,
            memory.clone(),
            tx,
            cancel,
            &workspace_root,
            &security,
            &mut tool_calls_made,
            &mut wrote_files,
            tool_hooks,
            user_hooks,
            session_id,
            quality_findings,
            &mut quality_blocked,
            permission.as_ref(),
            approval.as_ref(),
            ask_without_responder_deny,
            &extensions,
            concurrent_tools,
            tool_cache,
            &mut tool_cache_map,
            observe.as_ref(),
            &mut metrics,
            &mut phase_runner,
            &protocol_gates,
            // A1：复用步内快照作为本步 provider 请求的消息序列
            // （压缩路径已在修改后重新快照）。
            &snapshot,
        )
        .await?;

        match step_result {
            StepOutcome::Complete(output) => {
                // ── P1 完成前确定性验证：有文件写入才触发；bash 缺失或未配置降级放行 ──
                if let Some(vs) = verify_settings.as_ref() {
                    // F10：verify 相位起点移入 `wrote_files && !commands` 分支内
                    // （与 run_verify_pass 同条件）——无写文件/无验证命令时不
                    // 产出空 verify 相位（此前无条件 phase_enter 会在报告中
                    // 留一个空相位）。review 相位已在 `wrote_files` 分支内，
                    // 无同类问题。
                    if wrote_files && !vs.commands.is_empty() {
                        // 任务质量闭环 B：verify 相位起点（报告阶段时间戳）。
                        diagnose.phase_enter("verify");
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
                            crate::verify::VerifyOutcome::Pass => {
                                metrics.observe_verify(true);
                                // 协议门控（阶段3）：verify pass → Verify 阶段
                                // 边界（verify-evidence 门在此快照评估，此时
                                // 计数含本轮 passed → 通过）。
                                if protocol_active {
                                    phase_runner.observe_verify(true);
                                    // Bugbot #12：findings 接会话质量快照。
                                    let findings = quality_findings.lock().await.clone();
                                    let ctx = phase_runner.build_ctx(Phase::Verify, true, findings);
                                    let violations = phase_runner.transition(
                                        Phase::Verify,
                                        &protocol_gates,
                                        &ctx,
                                    );
                                    tx.send(Ok(RunEvent::PhaseTransition {
                                        transition: PhaseTransition {
                                            phase: Phase::Verify,
                                            outcome: if violations.is_empty() {
                                                PhaseOutcome::Pass
                                            } else {
                                                PhaseOutcome::Violated
                                            },
                                        },
                                    }))
                                    .await
                                    .ok();
                                    for v in &violations {
                                        tx.send(Ok(RunEvent::GateViolation(v.clone()))).await.ok();
                                    }
                                    let (pv, pt) = phase_runner.stats();
                                    metrics.sync_protocol_stats(pv, pt);
                                }
                            }
                            crate::verify::VerifyOutcome::Fail(reason)
                                if verify_cycles < vs.max_cycles =>
                            {
                                verify_cycles += 1;
                                metrics.observe_verify(false);
                                if protocol_active {
                                    phase_runner.observe_verify(false);
                                }
                                metrics.observe_retry();
                                let original = verify_failure_message(&reason);
                                // 任务质量闭环 B：reflect 相位起点（报告阶段时间戳）。
                                diagnose.phase_enter("reflect");
                                let (content, reflection) = reflect_retry(
                                    &reflect_settings,
                                    &lesson_hook,
                                    &input.prompt,
                                    &reason,
                                    &output.text,
                                    original.clone(),
                                )
                                .await;
                                // 任务质量闭环 B：反思产物进诊断（root_cause/fix_plan）。
                                if let Some(r) = reflection {
                                    diagnose.record_reflection(r);
                                }
                                // 任务质量闭环 C：retry 文案发生反思改写才计为
                                // 失败路径上有 reflection 记录（compose_retry_message
                                // 恒前置 [Reflection] 前缀，可直接比较）。
                                if content != original {
                                    metrics.observe_reflection();
                                }
                                memory.write().await.add_message(Message {
                                    role: Role::User,
                                    content,
                                    name: None,
                                    tool_calls: None,
                                    tool_call_id: None,
                                    reasoning_content: None,
                                });
                                continue; // 回炉修复，下一次 Complete 再验证
                            }
                            crate::verify::VerifyOutcome::Fail(reason) => {
                                metrics.observe_verify(false);
                                if protocol_active {
                                    phase_runner.observe_verify(false);
                                }
                                metrics.emit(None);
                                let reason = attribute_pause_reason(
                                    attribution_settings.as_ref(),
                                    &attribution_budget,
                                    &input.prompt,
                                    &reason,
                                    format!("verify_failed: {reason}"),
                                )
                                .await;
                                // 任务质量闭环 B：verify 达上限的 Paused 显式产出
                                // 报告（outcome=paused，失败详情带最终 pause reason）。
                                diagnose.record_failure("verify", None, None, reason.clone());
                                // Bugbot #2：Paused 终端分支同样接对抗审查。
                                wire_session_adversarial_review(
                                    &mut diagnose,
                                    provider.clone(),
                                    adversarial_review_enabled,
                                    &input.prompt,
                                    &*memory.read().await,
                                    quality_findings,
                                    quality_start_len,
                                )
                                .await;
                                diagnose
                                    .emit("paused", &memory.read().await.get_all())
                                    .await;
                                tx.send(Ok(RunEvent::Paused {
                                    reason,
                                    session_id: session_label.clone(),
                                }))
                                .await
                                .ok();
                                return Ok(());
                            }
                            crate::verify::VerifyOutcome::Skipped => {}
                        }
                    }
                    // ── P1b 完成前 LLM 验证：确定性命令通过后（或未配置命令时）
                    // 用 LLM 判定产出是否满足任务；默认关，调用/解析失败优雅跳过 ──
                    if wrote_files {
                        if let Some(vp) = &vs.llm_provider {
                            match crate::verify::run_llm_verify_pass(
                                vp.as_ref(),
                                &input.prompt,
                                &output.text,
                                vs.llm_max_chars,
                            )
                            .await
                            {
                                crate::verify::VerifyOutcome::Pass => {
                                    metrics.observe_verify(true);
                                    // 协议门控（阶段3）：LLM verify 只同步计数
                                    // （Verify 阶段边界已由确定性 verify Pass 产出）。
                                    if protocol_active {
                                        phase_runner.observe_verify(true);
                                    }
                                }
                                crate::verify::VerifyOutcome::Fail(reason)
                                    if verify_cycles < vs.max_cycles =>
                                {
                                    verify_cycles += 1;
                                    metrics.observe_verify(false);
                                    if protocol_active {
                                        phase_runner.observe_verify(false);
                                    }
                                    metrics.observe_retry();
                                    memory.write().await.add_message(Message {
                                        role: Role::User,
                                        content: verify_failure_message(&reason),
                                        name: None,
                                        tool_calls: None,
                                        tool_call_id: None,
                                        reasoning_content: None,
                                    });
                                    continue; // 回炉修复，下一次 Complete 再验证
                                }
                                crate::verify::VerifyOutcome::Fail(reason) => {
                                    metrics.observe_verify(false);
                                    if protocol_active {
                                        phase_runner.observe_verify(false);
                                    }
                                    metrics.emit(None);
                                    let reason = attribute_pause_reason(
                                        attribution_settings.as_ref(),
                                        &attribution_budget,
                                        &input.prompt,
                                        &reason,
                                        format!("verify_failed: {reason}"),
                                    )
                                    .await;
                                    // 任务质量闭环 B：LLM verify 达上限的 Paused
                                    // 显式产出报告（outcome=paused）。
                                    diagnose.record_failure("verify", None, None, reason.clone());
                                    // Bugbot #2：Paused 终端分支同样接对抗审查。
                                    wire_session_adversarial_review(
                                        &mut diagnose,
                                        provider.clone(),
                                        adversarial_review_enabled,
                                        &input.prompt,
                                        &*memory.read().await,
                                        quality_findings,
                                        quality_start_len,
                                    )
                                    .await;
                                    diagnose
                                        .emit("paused", &memory.read().await.get_all())
                                        .await;
                                    tx.send(Ok(RunEvent::Paused {
                                        reason,
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
                }
                // ── B3 完成前自审：有文件写入才触发；质量系统在场（注册了
                // tool_hook）时要求 Blocking finding（质量闭环 A 的 review
                // 短路：无 Blocking finding 直接跳过），质量系统缺席
                // （tool_hooks 为空）时照旧自审；降级路径一律放行 Done ──
                if let (Some(rp), Some(rs)) = (&review_provider, &review_settings) {
                    if wrote_files && (tool_hooks.is_empty() || quality_blocked) {
                        // 任务质量闭环 B：review 相位起点（报告阶段时间戳）。
                        diagnose.phase_enter("review");
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
                                metrics.observe_review_pass();
                                if review_cycles > 0 {
                                    bump("fix_succeeded");
                                }
                            }
                            ReviewOutcome::Issues(issues) if review_cycles < rs.max_cycles => {
                                review_cycles += 1;
                                metrics.observe_retry();
                                metrics.observe_review_issues();
                                let original = crate::review::render_feedback(&issues);
                                // 任务质量闭环 B：reflect 相位起点（报告阶段时间戳）。
                                diagnose.phase_enter("reflect");
                                let (content, reflection) = reflect_retry(
                                    &reflect_settings,
                                    &lesson_hook,
                                    &input.prompt,
                                    &issues.join("; "),
                                    &output.text,
                                    original.clone(),
                                )
                                .await;
                                // 任务质量闭环 B：反思产物进诊断（root_cause/fix_plan）。
                                if let Some(r) = reflection {
                                    diagnose.record_reflection(r);
                                }
                                if content != original {
                                    metrics.observe_reflection();
                                }
                                memory.write().await.add_message(Message {
                                    role: Role::User,
                                    content,
                                    name: None,
                                    tool_calls: None,
                                    tool_call_id: None,
                                    reasoning_content: None,
                                });
                                continue; // 回炉修复，下一次 Complete 再审
                            }
                            ReviewOutcome::Issues(issues) => {
                                metrics.observe_review_issues();
                                metrics.emit(None);
                                let head = issues
                                    .iter()
                                    .take(3)
                                    .cloned()
                                    .collect::<Vec<_>>()
                                    .join("; ");
                                let reason = attribute_pause_reason(
                                    attribution_settings.as_ref(),
                                    &attribution_budget,
                                    &input.prompt,
                                    &head,
                                    format!("review_issues: {head}"),
                                )
                                .await;
                                // 任务质量闭环 B：review 达上限的 Paused 显式产出
                                // 报告（outcome=paused，失败详情带最终 pause reason）。
                                diagnose.record_failure("review", None, None, reason.clone());
                                diagnose
                                    .emit("paused", &memory.read().await.get_all())
                                    .await;
                                tx.send(Ok(RunEvent::Paused {
                                    reason,
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
                // 任务质量闭环 B：Complete 分支结束（成功或取消）统一
                // suppress 诊断守卫，禁止 Drop 兜底产出报告——取消是正常
                // 终止（metrics 已计 Cancelled），与成功一样不产诊断报告；
                // Drop 兜底仅覆盖「异常/panic 终止」路径。
                //
                // 协议阶段3 例外：协议启用 + verify 已配置 + 会话无任何
                // passed 验证证据（verify-evidence 硬门未通过）时**不**
                // suppress，产 `DiagnoseReport { outcome: "unverified" }`
                // 报告（证据链判定，spec §4.1）；verify-evidence 通过时
                // 维持现状（suppress，不产报告）。
                let mut unverified = false;
                if protocol_active && !cancel.is_cancelled() {
                    // 会话收尾：Distill 阶段边界 + 门快照（verify-evidence /
                    // distill-on-complex 在此求值）。lesson 代理：本 run 有
                    // 反思记录即视为已产出 lesson（memory_distill 钩子在
                    // run_stream 层，主循环不可见）。
                    phase_runner.set_has_lesson(metrics.reflection_count > 0);
                    // Bugbot #12：findings 接会话质量快照。
                    let findings = quality_findings.lock().await.clone();
                    let ctx =
                        phase_runner.build_ctx(Phase::Distill, verify_settings.is_some(), findings);
                    let violations = phase_runner.transition(Phase::Distill, &protocol_gates, &ctx);
                    tx.send(Ok(RunEvent::PhaseTransition {
                        transition: PhaseTransition {
                            phase: Phase::Distill,
                            outcome: if violations.is_empty() {
                                PhaseOutcome::Pass
                            } else {
                                PhaseOutcome::Violated
                            },
                        },
                    }))
                    .await
                    .ok();
                    for v in &violations {
                        tx.send(Ok(RunEvent::GateViolation(v.clone()))).await.ok();
                    }
                    let (pv, pt) = phase_runner.stats();
                    metrics.sync_protocol_stats(pv, pt);
                    if verify_settings.is_some() && !phase_runner.verify_evidence_passed() {
                        unverified = true;
                    }
                }
                // 对抗审查（阶段3）：会话收尾触发（Blocking finding / 敏感
                // 工具调用），**与 unverified 无关**（Bugbot #2 修正：原实现
                // 放在 unverified 分支内，verify 通过的成功会话——Blocking
                // finding 常见场景——直接 suppress，审查永不发生）。verify
                // 通过与否只影响报告 outcome；子代理不可用或条件不满足 →
                // 优雅跳过（warn），报告 adversarial_review 保持 None。
                wire_session_adversarial_review(
                    &mut diagnose,
                    provider.clone(),
                    adversarial_review_enabled,
                    &input.prompt,
                    &*memory.read().await,
                    quality_findings,
                    quality_start_len,
                )
                .await;
                if unverified {
                    diagnose
                        .emit("unverified", &memory.read().await.get_all())
                        .await;
                } else {
                    diagnose.suppress();
                }
                let outcome = if cancel.is_cancelled() {
                    RunOutcome::Cancelled
                } else {
                    RunOutcome::Completed
                };
                metrics.emit(Some(outcome));
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
                    metrics.emit(Some(RunOutcome::PausedMaxSteps));
                    // 任务质量闭环 B：max-steps 的 Paused 显式产出报告。
                    diagnose.record_failure(
                        "tool",
                        None,
                        None,
                        format!("reached max steps ({max_steps})"),
                    );
                    // Bugbot #2：Paused 终端分支同样接对抗审查。
                    wire_session_adversarial_review(
                        &mut diagnose,
                        provider.clone(),
                        adversarial_review_enabled,
                        &input.prompt,
                        &*memory.read().await,
                        quality_findings,
                        quality_start_len,
                    )
                    .await;
                    diagnose
                        .emit("paused", &memory.read().await.get_all())
                        .await;
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
        metrics.emit(Some(RunOutcome::PausedMaxSteps));
        // 任务质量闭环 B：max-steps 的 Paused 显式产出报告。
        diagnose.record_failure(
            "tool",
            None,
            None,
            format!("reached max steps ({max_steps})"),
        );
        // Bugbot #2：Paused 终端分支同样接对抗审查。
        wire_session_adversarial_review(
            &mut diagnose,
            provider.clone(),
            adversarial_review_enabled,
            &input.prompt,
            &*memory.read().await,
            quality_findings,
            quality_start_len,
        )
        .await;
        diagnose
            .emit("paused", &memory.read().await.get_all())
            .await;
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
    memory: Arc<tokio::sync::RwLock<Memory>>,
    tx: &mpsc::Sender<anyhow::Result<RunEvent>>,
    cancel: &CancellationToken,
    workspace_root: &std::path::Path,
    security: &SecurityContext,
    tool_calls_made: &mut usize,
    wrote_files: &mut bool,
    tool_hooks: &[Arc<dyn ToolHook>],
    user_hooks: &UserHooks,
    session_id: &str,
    quality_findings: &Arc<tokio::sync::Mutex<Vec<QualityFinding>>>,
    quality_blocked: &mut bool,
    permission: Option<&Arc<PermissionGate>>,
    approval: Option<&Arc<dyn ApprovalResponder>>,
    ask_without_responder_deny: bool,
    extensions: &[Arc<ExtensionApplier>],
    concurrent_tools: bool,
    tool_cache_enabled: bool,
    tool_cache: &mut HashMap<u64, String>,
    observe: Option<&ObserveSettings>,
    metrics: &mut MetricsGuard,
    phase_runner: &mut crate::phase_runner::PhaseRunner,
    protocol_gates: &[Arc<dyn PhaseGate>],
    // A1：本步消息快照（由调用方在步开始时取，压缩后重新快照）。
    // provider 请求复用此快照，不再在步内重复全量克隆。
    messages: &[Message],
) -> anyhow::Result<StepOutcome> {
    // Build tool refs for provider
    let tool_refs: Vec<&dyn Tool> = tools.iter().map(|t| t.as_ref()).collect();

    // DeepSeek V4 protocol — ValidatedRequest::new fails early with
    // structured violation list, preventing corrupt messages from
    // ever reaching the provider
    let validated = deepseeknova_provider::ValidatedRequest::new(messages, &tool_refs).map_err(
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
        memory.write().await.add_message(Message {
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

        memory.write().await.add_message(Message {
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

        // ── 协议门控（阶段3）：Execute 阶段边界（工具执行前）──
        // 本轮已产出文本 → 会话级 has_plan_text 置位（单调）。
        // Blocking violation → 拒绝本轮全部工具执行（复用既有 Deny 语义：
        // 工具结果回填 "blocked by protocol gate" 错误，模型可见；不产生
        // 悬空 tool_calls，保住 replay 不变量）。
        // Bugbot #3 修正：drift 二次不再产 Blocking（阶段级无 Ask 桥可用），
        // 降级为 Warning + 「需人工确认」标注（见 phase_runner.rs transition
        // 注释与 spec §13 #2/#4 修正记录）——此处仅真实 Blocking（如
        // verify-evidence 硬门）触发全轮 deny。
        let mut protocol_block: Option<String> = None;
        if !protocol_gates.is_empty() {
            phase_runner.set_has_plan_text(!text_buf.is_empty());
            // Bugbot #12：findings 接会话质量快照。
            let findings = quality_findings.lock().await.clone();
            let ctx = phase_runner.build_ctx(Phase::Execute, false, findings);
            let violations = phase_runner.transition(Phase::Execute, protocol_gates, &ctx);
            tx.send(Ok(RunEvent::PhaseTransition {
                transition: PhaseTransition {
                    phase: Phase::Execute,
                    outcome: if violations.is_empty() {
                        PhaseOutcome::Pass
                    } else {
                        PhaseOutcome::Violated
                    },
                },
            }))
            .await
            .ok();
            for v in &violations {
                tx.send(Ok(RunEvent::GateViolation(v.clone()))).await.ok();
            }
            let (pv, pt) = phase_runner.stats();
            metrics.sync_protocol_stats(pv, pt);
            if violations
                .iter()
                .any(|v| v.severity == FindingSeverity::Blocking)
            {
                protocol_block = Some("blocked by protocol gate".to_string());
            }
        }

        // ── P1 执行调度：权限预检先行，读类并发、写类保序串行 ──
        // 预检按原始顺序串行执行（Ask 等待用户，避免并发弹窗），随后按
        // `read_only` 分段：段内只读工具并发（JoinSet），写工具独占段串行。
        let mut decisions: Vec<Option<String>> = Vec::with_capacity(pending_calls.len());
        for call in &pending_calls {
            let mut gate_block: Option<String> = protocol_block.clone();
            if gate_block.is_none() {
                gate_block = match permission {
                    Some(gate) => {
                        // 完整裁决：reason + "拒绝即教育"建议随阻断文案透出。
                        let verdict = match tool_map.get(&call.name) {
                            Some(tool) => gate.check(tool.as_ref(), &call.arguments),
                            None => CheckVerdict::allow(),
                        };
                        match verdict.decision() {
                            Decision::Allow => None,
                            Decision::Deny => {
                                let mut msg = verdict.reason().to_string();
                                if verdict.is_hard_deny() {
                                    msg.push_str(" (安全硬拒，不可通过规则覆盖)");
                                }
                                let sug = render_suggestions(verdict.suggestions());
                                if !sug.is_empty() {
                                    msg.push_str(&format!("\n{sug}"));
                                }
                                Some(msg)
                            }
                            Decision::Ask => {
                                // 返回 (是否放行, 拒绝原因)：responder 存在时拒绝原因
                                // 由调用方（用户取消/拒绝）决定，兜底 deny 时给出
                                // 明确的 fail-closed 说明。
                                let (approved, deny_reason) = if let Some(responder) = approval {
                                    let approval_id = format!("approval_{}", uuid::Uuid::new_v4());
                                    // 风险标签同时进 RunEvent 描述（serve/桌面）
                                    // 与 responder 描述（TUI 审批浮层直接消费）。
                                    let mut request_desc = call.arguments.clone();
                                    if let Some(risk) = approval_risk_prefix(
                                        permission.map(|g| g.as_ref()),
                                        &call.name,
                                        &call.arguments,
                                    ) {
                                        request_desc = format!("{risk}\n{request_desc}");
                                    }
                                    let mut description = request_desc.clone();
                                    let sug = render_suggestions(verdict.suggestions());
                                    if !sug.is_empty() {
                                        description.push_str(&format!("\n\n{sug}"));
                                    }
                                    tx.send(Ok(RunEvent::ApprovalRequest {
                                        id: approval_id.clone(),
                                        title: format!("Allow tool: {}", call.name),
                                        description: Some(description),
                                    }))
                                    .await
                                    .ok();
                                    // Block until the user answers, but never
                                    // deadlock: cancellation resolves to a denial.
                                    let ans = tokio::select! {
                                        ans = responder.request(
                                            &approval_id,
                                            &call.name,
                                            Some(&request_desc),
                                        ) => ans,
                                        _ = cancel.cancelled() => false,
                                    };
                                    (ans, None)
                                } else if ask_without_responder_deny {
                                    // 无 responder（库级/裸 Agent/未接线交互面）且未显式
                                    // 配置 allow：默认 fail-closed 拒绝——非交互/库级调用
                                    // 没有人工审批通道，放行写操作属 fail-open。与子代理
                                    // 侧（sub_agent.rs Ask 一律视作拒绝）语义对齐。
                                    tracing::warn!(
                                        security_event = "ask_denied_no_responder",
                                        tool = %call.name,
                                        "Ask denied: no approval responder wired (fail-closed); \
                                         set permissions.ask_without_responder = \"allow\" to \
                                         auto-allow"
                                    );
                                    (
                                        false,
                                        Some(
                                            "denied: no approval responder available \
                                             (fail-closed)"
                                                .to_string(),
                                        ),
                                    )
                                } else {
                                    // 显式配置 ask_without_responder = "allow"：恢复旧的
                                    // 自动放行契约，仍需记录安全事件供审计。
                                    tracing::warn!(
                                        security_event = "ask_auto_allowed_no_responder",
                                        tool = %call.name,
                                        "Ask auto-allowed: no approval responder wired and \
                                         permissions.ask_without_responder = \"allow\""
                                    );
                                    (true, None)
                                };
                                if approved {
                                    gate.cache_decision(
                                        &call.name,
                                        &call.arguments,
                                        Decision::Allow,
                                    );
                                    None
                                } else {
                                    deny_reason.or_else(|| Some("denied by user".to_string()))
                                }
                            }
                        }
                    }
                    None => None,
                };
            }
            // ── 任务质量闭环 A：ToolHook before 预检（gate 决策之后串行执行）。
            // 决策合并：任一 Deny → 拒绝；无 Deny 且任一 Ask → 走 approval 桥
            // （与 gate Ask 同路径）；全 Allow → 放行。
            // F3 契约变更（fail-closed）：`interested()` 与 `before()` 均在
            // `catch_unwind` 内执行；任一 panic → 按 `HookVerdict::Deny` 拒绝
            // 执行（安全判定 fail-closed，warn 注明）。注意：`core/src/tool_hook.rs`
            // 的 trait 文档仍写 fail-open 旧契约，需由 core 侧同步（见任务报告）。
            if gate_block.is_none() && !tool_hooks.is_empty() {
                let hook_call = ToolCall {
                    id: call.id.clone(),
                    ty: "function".to_string(),
                    function: FunctionCall {
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    },
                };
                let ctx = ToolHookCtx { workspace_root };
                let mut hook_deny: Option<String> = None;
                let mut hook_ask: Option<String> = None;
                for hook in tool_hooks {
                    // F3a：interested 同样进 catch_unwind——panic 按未注册处理
                    // （跳过该钩子），不让单个钩子的崩溃炸掉整个 run。
                    let interested = catch_unwind(AssertUnwindSafe(|| hook.interested(&hook_call)))
                        .unwrap_or_else(|_| {
                            warn!(
                                "tool hook '{}' panicked in interested(); treated as not interested",
                                hook.name()
                            );
                            false
                        });
                    if !interested {
                        continue;
                    }
                    let verdict = catch_unwind(AssertUnwindSafe(|| hook.before(&ctx, &hook_call)))
                        .unwrap_or_else(|_| {
                            // F3b：before panic → deny（fail-closed）。
                            warn!(
                                "tool hook '{}' panicked in before() → deny (fail-closed)",
                                hook.name()
                            );
                            HookVerdict::Deny(format!(
                                "tool hook '{}' panicked during pre-check (fail-closed deny)",
                                hook.name()
                            ))
                        });
                    match verdict {
                        HookVerdict::Allow | HookVerdict::AllowWith(_) => {}
                        HookVerdict::Deny(reason) => {
                            hook_deny = Some(reason);
                            break;
                        }
                        HookVerdict::Ask(reason) => hook_ask = Some(reason),
                    }
                }
                if let Some(reason) = hook_deny {
                    gate_block = Some(reason);
                } else if let Some(reason) = hook_ask {
                    let (approved, deny_reason) = if let Some(responder) = approval {
                        let approval_id = format!("approval_{}", uuid::Uuid::new_v4());
                        tx.send(Ok(RunEvent::ApprovalRequest {
                            id: approval_id.clone(),
                            title: format!("Allow tool: {}", call.name),
                            description: Some(reason),
                        }))
                        .await
                        .ok();
                        let ans = tokio::select! {
                            ans = responder.request(
                                &approval_id,
                                &call.name,
                                Some(&call.arguments),
                            ) => ans,
                            _ = cancel.cancelled() => false,
                        };
                        (ans, None)
                    } else if ask_without_responder_deny {
                        // 与 gate 路径同款 fail-closed 兜底：无 responder 默认拒绝。
                        tracing::warn!(
                            security_event = "ask_denied_no_responder",
                            tool = %call.name,
                            "Ask denied: no approval responder wired (fail-closed); set \
                             permissions.ask_without_responder = \"allow\" to auto-allow"
                        );
                        (
                            false,
                            Some(
                                "denied: no approval responder available (fail-closed)".to_string(),
                            ),
                        )
                    } else {
                        tracing::warn!(
                            security_event = "ask_auto_allowed_no_responder",
                            tool = %call.name,
                            "Ask auto-allowed: no approval responder wired and \
                             permissions.ask_without_responder = \"allow\""
                        );
                        (true, None)
                    };
                    if !approved {
                        gate_block = deny_reason.or_else(|| Some("denied by user".to_string()));
                    }
                }
            }
            // ── 用户级外部 hooks：tool_before（额外一层，fail-closed）──
            // 内部 tool_hook 链 + 用户 hooks 都过才执行（AND 链）。任一命令
            // 非 0 退出 / 超时 / 崩溃，或 stdout 裁决 `allowed=false` → 阻止
            // 执行（原因透传给调用方）。放行语义与内部链独立叠加。
            if gate_block.is_none() && !user_hooks.tool_before.is_empty() {
                let payload = HookPayload {
                    event: HookEvent::ToolBefore.as_str(),
                    tool: Some(&call.name),
                    arguments: Some(&call.arguments),
                    workspace: workspace_root,
                    session_id,
                };
                for cmd in &user_hooks.tool_before {
                    let run = run_user_hook(cmd, &payload);
                    if !run.exec.is_allowed() {
                        gate_block = Some(format!(
                            "blocked by user hook '{}' (fail-closed: {:?})",
                            cmd.command, run.exec
                        ));
                        break;
                    }
                    if let Some(v) = run.verdict {
                        if !v.allowed {
                            let reason = if v.reason.is_empty() {
                                format!("denied by user hook '{}'", cmd.command)
                            } else {
                                v.reason.clone()
                            };
                            gate_block = Some(reason);
                            break;
                        }
                    }
                }
            }
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
                        let ok = !is_tool_error_result(&result);
                        results[idx] = Some(result);
                        executed[idx] = true;
                        metrics.observe_tool_call(&pending_calls[idx].name, ok);
                        // Bugbot #4：drift-detection=off 时门从 builtin 列表
                        // 摘除，PhaseRunner 在 Execute transition 已探测到并
                        // 关闭计数——此处调用自然返回 None（不发 DriftFinding）。
                        if !protocol_gates.is_empty() {
                            if let Some(drift) =
                                phase_runner.note_tool_failure(&pending_calls[idx].name, ok)
                            {
                                tx.send(Ok(RunEvent::DriftFinding(drift))).await.ok();
                            }
                        }
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
                            let ok = !is_tool_error_result(&result);
                            results[idx] = Some(result);
                            executed[idx] = true;
                            metrics.observe_tool_call(&pending_calls[idx].name, ok);
                            // Bugbot #4：同顺序路径，drift-off 时返回 None。
                            if !protocol_gates.is_empty() {
                                if let Some(drift) =
                                    phase_runner.note_tool_failure(&pending_calls[idx].name, ok)
                                {
                                    tx.send(Ok(RunEvent::DriftFinding(drift))).await.ok();
                                }
                            }
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
                            if !is_tool_error_result(r) && !r.starts_with("[cached]") {
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
            let mut result = results[i].clone().unwrap_or_else(|| {
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
            // 编辑后诊断：写类工具执行成功后，自动调用 lsp_diagnostics 并把
            // 诊断注入 ToolResult（语言服务器未安装/被禁用时静默跳过，不制造噪音）。
            if executed[i]
                && matches!(call.name.as_str(), "write_file" | "edit_file" | "move_file")
                && !is_tool_error_result(&result)
            {
                if let Some(lsp) = tool_map.get("lsp_diagnostics") {
                    if let Some(path) = extract_tool_path(&call.arguments) {
                        let lsp_ctx = build_tool_context(
                            "lsp-post-edit",
                            cancel.child_token(),
                            workspace_root,
                            security,
                            extensions,
                        );
                        let lsp_args = serde_json::json!({ "path": path }).to_string();
                        match lsp.execute(&lsp_ctx, &lsp_args).await {
                            Ok(diag)
                                if diag.starts_with("LSP diagnostics")
                                    || diag.starts_with("No LSP diagnostics") =>
                            {
                                result.push_str("\n\n---\n");
                                result.push_str(&diag);
                            }
                            _ => {}
                        }
                    }
                }
            }
            // P2.2 观察压缩：事件透出原始结果；历史存压缩摘要（压缩失败回退原始截断）。
            let stored = if let Some(obs) = observe {
                if result.len() > obs.threshold_chars
                    && !is_tool_error_result(&result)
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
            // ── 任务质量闭环 A：ToolHook after 写后评估（执行成功才评估）。
            // findings 进事件流 + 会话级累计；Blocking 置位 review 短路标志。
            // F3：after 的 panic 保持 fail-open（空 findings + warn，不影响
            // 执行）；`interested()` 同样进 catch_unwind（panic 按未注册处理）。
            if executed[i] && !is_tool_error_result(&result) && !tool_hooks.is_empty() {
                let hook_call = ToolCall {
                    id: call.id.clone(),
                    ty: "function".to_string(),
                    function: FunctionCall {
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    },
                };
                let ctx = ToolHookCtx { workspace_root };
                for hook in tool_hooks {
                    let interested =
                        catch_unwind(AssertUnwindSafe(|| hook.interested(&hook_call)))
                            .unwrap_or_else(|_| {
                                warn!(
                                    "tool hook '{}' panicked in interested(); treated as not interested",
                                    hook.name()
                                );
                                false
                            });
                    if !interested {
                        continue;
                    }
                    let findings =
                        catch_unwind(AssertUnwindSafe(|| hook.after(&ctx, &hook_call, &result)))
                            .unwrap_or_else(|_| {
                                warn!(
                                    "tool hook '{}' panicked in after(); fail-open empty findings",
                                    hook.name()
                                );
                                Vec::new()
                            });
                    if findings.is_empty() {
                        continue;
                    }
                    // F9：会话级 findings 有界累积——超限丢弃新 finding 并仅
                    // warn 一次（长会话保护；避免 Vec::remove(0) 的 O(n) 搬迁）。
                    // 事件流照常发出（用户可见），仅不入会话累计。与 MetricsGuard
                    // 的 run 级差分切片（F4）兼容：切片起点为 run 开始时长度，
                    // 丢弃只会让切片更短，不会混入他 run 数据。
                    let mut dropped_warned = false;
                    let mut emitted = Vec::with_capacity(findings.len());
                    {
                        let mut qf = quality_findings.lock().await;
                        for finding in findings {
                            if finding.severity == FindingSeverity::Blocking {
                                *quality_blocked = true;
                            }
                            if qf.len() >= MAX_QUALITY_FINDINGS {
                                if !dropped_warned {
                                    warn!(
                                        "quality findings exceeded cap ({}); dropping new findings",
                                        MAX_QUALITY_FINDINGS
                                    );
                                    dropped_warned = true;
                                }
                            } else {
                                qf.push(finding.clone());
                            }
                            emitted.push(finding);
                        }
                    }
                    for finding in emitted {
                        tx.send(Ok(RunEvent::QualityFinding(finding))).await.ok();
                    }
                }
            }
            // ── 用户级外部 hooks：tool_after（已执行；失败仅 warn，不阻断）──
            // 工具已执行，通知命令失败不影响主流程（与内部 after fail-open
            // 语义一致）。executed 判定不含工具自身错误结果——工具即使返回
            // 错误也视为「已执行」，通知命令照常触发。
            if executed[i] && !user_hooks.tool_after.is_empty() {
                let payload = HookPayload {
                    event: HookEvent::ToolAfter.as_str(),
                    tool: Some(&call.name),
                    arguments: Some(&call.arguments),
                    workspace: workspace_root,
                    session_id,
                };
                fire_user_notify_hooks(&user_hooks.tool_after, &payload);
            }
            memory.write().await.add_message(Message {
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

/// 工具结果是否呈错误形态（供 metrics 计数 / 缓存回填 / 观察压缩分流）。
/// 保守策略：只识别明确错误形态——`Error:` / `error:` 前缀（trim 后）、
/// 以及工具返回的错误 JSON（`{"error": ...}` / `{"success": false, ...}`
/// 开头形态）；`{"error": null|false}` 与 `{"success": true}` 不判错，
/// 其余正常输出一律计为成功。
pub(crate) fn is_tool_error_result(result: &str) -> bool {
    let s = result.trim_start();
    if s.starts_with("Error:") || s.starts_with("error:") {
        return true;
    }
    // 错误 JSON：仅检查首个字段（开头形态），字段值需明确指向错误。
    let Some(rest) = s
        .strip_prefix('{')
        .map(str::trim_start)
        .and_then(|r| r.strip_prefix('"'))
    else {
        return false;
    };
    let Some(end) = rest.find('"') else {
        return false;
    };
    let field = &rest[..end];
    let Some(value) = rest[end + 1..]
        .trim_start()
        .strip_prefix(':')
        .map(str::trim_start)
    else {
        return false;
    };
    if field.eq_ignore_ascii_case("error") {
        // `{"error": ...}`：null/false 值不算明确错误，其余值视为错误形态。
        return !value.starts_with("null") && !value.starts_with("false");
    }
    if field.eq_ignore_ascii_case("success") {
        // `{"success": false, ...}`：仅显式 false 判错。
        return value.starts_with("false");
    }
    false
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

/// 结果文本是否含错误指示（宽松 contains 语义，供机械续步分类，与
/// `is_tool_error_result` 的整体判定互补）：大小写不敏感的 `error:` 片段、
/// JSON `"error"` 键出现、以及 `{"success": false}` 显式 false 值。
/// 宁可多判错误（→ high，更强模型），不漏判错误走 quick。
fn contains_error_signal(text: &str) -> bool {
    if text.to_ascii_lowercase().contains("error:") {
        return true;
    }
    // JSON `{"error": ...}` 形态：`"error"` 键（后随 `:`）出现即视为错误指示
    // （宽松判定，null/false 值也判错；与 is_tool_error_result 的首字段
    // null/false 特判互补）。字符串值位置的 `"error"`（后随 `}`/`,`）不判。
    let mut rest = text;
    while let Some(idx) = rest.find("\"error\"") {
        let after = rest[idx + "\"error\"".len()..].trim_start();
        if after.starts_with(':') {
            return true;
        }
        rest = after;
    }
    // `{"success": false, ...}`：`"success"` 键后紧跟 `: false`。
    let mut rest = text;
    while let Some(idx) = rest.find("\"success\"") {
        let after = &rest[idx + "\"success\"".len()..];
        let after = after.trim_start();
        if let Some(after) = after.strip_prefix(':') {
            return after.trim_start().starts_with("false");
        }
        rest = after;
    }
    false
}

/// P2.1 每步分类：上一条消息是工具结果且不含错误信号 → 机械续步（quick）；
/// 其余（首步、出错、回炉反馈）→ high。错误识别与 `is_tool_error_result`
/// 语义一致（大小写不敏感 `error:` + 错误 JSON 形态），但保持 contains
/// 语义（长输出中任何位置出现即算错误信号）。
/// A1：入参改为消息序列快照（步内复用同一快照，不再各自克隆内存）。
fn classify_quick_step(messages: &[Message]) -> bool {
    match messages.last() {
        Some(m) if m.role == Role::Tool => !contains_error_signal(&m.content),
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

/// 编辑后诊断用：从 write/edit/move 工具参数提取目标文件路径（`path` 字段）。
fn extract_tool_path(args: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(args).ok()?;
    v.get("path").and_then(|p| p.as_str()).map(str::to_string)
}

/// Observe 阶段工具输出压缩提示词（契约：保留事实/路径/退出码/数字，纯摘要输出）。
fn render_compression_prompt(tool: &str, raw: &str) -> String {
    format!(
        "You are the Observe stage of the Observe → Plan → Tool → Verify → \
         Reflect → Next Action loop. Compress the following tool output \
         (`{tool}`) into a concise structured summary. Preserve every fact, \
         file path, exit code and number. Output only the summary.\n\n{raw}"
    )
}

/// P2.2 观察压缩：用廉价模型把大输出压成结构化摘要；任何失败返回 None（回退截断）。
async fn compress_observation(obs: &ObserveSettings, tool: &str, raw: &str) -> Option<String> {
    let prompt = render_compression_prompt(tool, raw);
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

    /// 固定返回给定 provider 的 fake decider，记录被调用次数。
    struct FakeAutoDecider {
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        provider: Arc<dyn Provider>,
    }

    #[async_trait::async_trait]
    impl AutoRouteDecider for FakeAutoDecider {
        async fn decide(&self, _m: &[Message]) -> Option<Arc<dyn Provider>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Some(Arc::clone(&self.provider))
        }
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

    #[test]
    fn metrics_guard_lock_busy_at_start_emits_empty_findings() {
        // A1 回归：构造时 quality_findings 锁被占用 → start_len=None，
        // emit 报空 findings，绝不回退 0 把并发 run 的 findings 误切进来。
        let qf: Arc<tokio::sync::Mutex<Vec<QualityFinding>>> =
            Arc::new(tokio::sync::Mutex::new(Vec::new()));
        // 构造时锁被占用（blocking_lock 持有，try_lock 必失败）。
        let held = qf.blocking_lock();
        let emitted: Arc<std::sync::Mutex<Option<QualitySummary>>> =
            Arc::new(std::sync::Mutex::new(None));
        let hook: MetricsHook = {
            let emitted = Arc::clone(&emitted);
            Arc::new(move |_snap: SessionSnapshot, summary: QualitySummary| {
                *emitted.lock().unwrap() = Some(summary);
            })
        };
        let mut guard = MetricsGuard::new(Some(hook), &qf, None, &[], PathBuf::new());
        assert!(guard.start_len.is_none(), "锁忙时 start_len 应为 None");
        drop(held);
        // 之后（并发 run 视角）向共享容器追加 findings。
        qf.blocking_lock().push(QualityFinding {
            rule: "other-run".into(),
            severity: FindingSeverity::Warning,
            passed: false,
            evidence: "concurrent".into(),
        });
        guard.emit(Some(RunOutcome::Completed));
        let summary = emitted
            .lock()
            .unwrap()
            .take()
            .expect("hook 应被恰好调用一次");
        assert!(
            summary.findings.is_empty(),
            "锁忙启动的 run 不得把并发 findings 误切进本 run"
        );
    }

    #[test]
    fn tool_error_heuristic_flags_error_forms_only() {
        // 错误形态 → 判失败。
        for s in [
            "Error: boom",
            "error: boom",
            "  error: boom", // trim 后
            "Error: unknown tool 'x'",
            r#"{"error": "boom"}"#,
            r#"{"error":"boom"}"#,
            r#"{"error": {"code": 1}}"#,
            r#"{"success": false, "detail": "x"}"#,
            r#"{"success":false}"#,
            r#"{ "success" : false }"#,
        ] {
            assert!(is_tool_error_result(s), "应判为错误: {s}");
        }
        // 正常输出 → 判成功（不得误伤）。
        for s in [
            "all good",
            "Error handling code lives in src/error.rs",
            "erroneous result", // 非前缀
            "we got an error somewhere in the middle",
            r#"{"success": true, "data": 1}"#,
            r#"{"error": null}"#,
            r#"{"error": false}"#,
            r#"{"errorless": true}"#,
            r#"{"status": "error"}"#, // 非 error/success 键名不判错
            "",
        ] {
            assert!(!is_tool_error_result(s), "应判为成功: {s}");
        }
    }

    #[test]
    fn classify_quick_step_flags_error_signals_case_insensitive() {
        // 错误指示（含小写 error、JSON 形态）→ 非 quick（high）。
        for content in [
            "Error: boom",
            "error: boom",
            "lots of text then Error: boom",
            r#"{"error": "boom"}"#,
            r#"{"error":null}"#,
            r#"{"success": false, "detail": "x"}"#,
            "prefix\n{\"error\": 1}\nsuffix",
        ] {
            let mut mem = Memory::new();
            mem.add_message(Message {
                role: Role::Tool,
                content: content.to_string(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            });
            assert!(
                !classify_quick_step(&mem.get_all()),
                "含错误指示应判 high（非 quick）: {content}"
            );
        }
    }

    #[test]
    fn classify_quick_step_normal_output_stays_quick() {
        // 正常工具输出 → quick（机械续步）。
        for content in [
            "all good",
            "42 lines read",
            r#"{"success": true, "data": 1}"#,
            r#"{"status": "error"}"#, // 非 error/success 键名不判错
            "errorless result",
        ] {
            let mut mem = Memory::new();
            mem.add_message(Message {
                role: Role::Tool,
                content: content.to_string(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            });
            assert!(
                classify_quick_step(&mem.get_all()),
                "正常输出应判 quick: {content}"
            );
        }
    }

    #[test]
    fn classify_quick_step_non_tool_last_message_is_not_quick() {
        let mut mem = Memory::new();
        mem.add_message(Message {
            role: Role::User,
            content: "Error: boom".to_string(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });
        assert!(!classify_quick_step(&mem.get_all()), "非工具消息不判 quick");
        assert!(
            !classify_quick_step(&Memory::new().get_all()),
            "空记忆不判 quick"
        );
    }

    #[test]
    fn extract_tool_path_reads_write_arguments() {
        assert_eq!(
            extract_tool_path(r#"{"path":"src/main.rs"}"#).as_deref(),
            Some("src/main.rs")
        );
        // move_file 用 source/destination，不触发编辑后诊断（避免对改名目标误诊）。
        assert_eq!(
            extract_tool_path(r#"{"source":"a.rs","destination":"b.rs"}"#),
            None
        );
        assert_eq!(extract_tool_path("not json"), None);
    }

    #[test]
    fn node_failure_summary_truncates_and_formats() {
        use deepseeknova_core::executor::AttributionHook as _;
        use deepseeknova_core::graph::{NodeId, NodeOutput};

        let hook = crate::attribution::NodeFailureSummary;
        let cap = crate::attribution::MAX_ATTRIBUTION_INPUT_CHARS;

        // 超长输入 → 截断到上限（字符级）。
        let long = "x".repeat(5000);
        let out = hook.on_node_failure(&NodeId::from("n1"), &NodeOutput::Error(long));
        let s = out.expect("Error 输出应产生摘要");
        let prefix = "node n1 failed: ";
        assert!(s.starts_with(prefix));
        assert_eq!(s.len(), prefix.len() + cap, "超长输入应被截断");

        // 短输入 → 原样保留。
        let out = hook.on_node_failure(&NodeId::from("n2"), &NodeOutput::Error("boom".into()));
        assert_eq!(out.as_deref(), Some("node n2 failed: boom"));

        // 非 Error 形态 → None。
        assert!(hook
            .on_node_failure(&NodeId::from("n3"), &NodeOutput::Text("ok".into()))
            .is_none());
        assert!(hook
            .on_node_failure(&NodeId::from("n3"), &NodeOutput::ToolResult("ok".into()))
            .is_none());
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

    // -----------------------------------------------------------------------
    // 任务质量闭环 B：结构化失败诊断报告（DiagnoseReport）集成测试
    // -----------------------------------------------------------------------

    /// Paused 结束（max-steps）→ 恰好一份报告：outcome=paused、阶段时序单调、
    /// failures 非空、session_id 与标注一致。
    #[tokio::test]
    async fn diagnose_hook_emits_report_on_paused_failure() {
        let captured = Arc::new(std::sync::Mutex::new(
            Vec::<crate::diagnose::DiagnoseReport>::new(),
        ));
        let cap = captured.clone();
        let hook: DiagnoseHook = Arc::new(move |report| cap.lock().unwrap().push(report));
        let agent = looping_agent(2)
            .with_on_max_steps("pause")
            .with_session_label("sess-diag-1")
            .with_diagnose_hook(hook);

        let mut stream = agent
            .run_stream(RunInput {
                prompt: "loop forever".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        let reports = captured.lock().unwrap();
        assert_eq!(reports.len(), 1, "exactly one report per failed run");
        let r = &reports[0];
        assert_eq!(r.outcome, "paused");
        assert_eq!(r.session_id, "sess-diag-1");
        assert!(!r.phases.is_empty(), "phases must be recorded");
        for p in &r.phases {
            assert!(
                p.ended_at_ms >= p.started_at_ms,
                "phase {} ended < started",
                p.name
            );
            assert_eq!(p.duration_ms, p.ended_at_ms - p.started_at_ms);
        }
        assert!(!r.failures.is_empty(), "failures must be non-empty");
    }

    /// 成功结束 → diagnose_hook 不被调用（success 路径 suppress 生效）。
    #[tokio::test]
    async fn diagnose_hook_not_called_on_success() {
        let captured = Arc::new(std::sync::Mutex::new(
            Vec::<crate::diagnose::DiagnoseReport>::new(),
        ));
        let cap = captured.clone();
        let hook: DiagnoseHook = Arc::new(move |report| cap.lock().unwrap().push(report));
        let agent =
            Agent::new(Arc::new(MockProvider::text("all good")), 3).with_diagnose_hook(hook);

        let mut stream = agent
            .run_stream(RunInput {
                prompt: "say hi".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        assert!(
            captured.lock().unwrap().is_empty(),
            "success run must not emit a diagnose report"
        );
    }

    /// 错误结束（max-steps error 模式，run_agent_loop 返回 Err）→ Drop 兜底
    /// 产出 outcome=failed 的报告。
    #[tokio::test]
    async fn diagnose_hook_reports_failed_on_error_mode() {
        let captured = Arc::new(std::sync::Mutex::new(
            Vec::<crate::diagnose::DiagnoseReport>::new(),
        ));
        let cap = captured.clone();
        let hook: DiagnoseHook = Arc::new(move |report| cap.lock().unwrap().push(report));
        let agent = looping_agent(2)
            .with_on_max_steps("error")
            .with_diagnose_hook(hook);

        let mut stream = agent
            .run_stream(RunInput {
                prompt: "loop forever".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        let reports = captured.lock().unwrap();
        assert_eq!(reports.len(), 1, "error path must emit via Drop fallback");
        assert_eq!(reports[0].outcome, "failed");
        assert!(
            !reports[0].failures.is_empty(),
            "Drop fallback must record the abnormal termination"
        );
    }

    /// 协议阶段3：协议启用 + verify 配置 + 无 passed 证据 → Complete 仍产出
    /// outcome=unverified 报告（证据链判定，spec §4.1）；协议未启用时维持
    /// 现状（suppress，不产报告）。
    #[tokio::test]
    async fn diagnose_hook_emits_unverified_when_protocol_enabled_without_verify_evidence() {
        // 无工具调用 → 无 wrote_files → verify 不执行 → 零 Verification 事件。
        let captured = Arc::new(std::sync::Mutex::new(
            Vec::<crate::diagnose::DiagnoseReport>::new(),
        ));
        let cap = captured.clone();
        let hook: DiagnoseHook = Arc::new(move |report| cap.lock().unwrap().push(report));

        let gates = crate::phase_runner::builtin_phase_gates(&HashMap::new());
        let agent = Agent::new(Arc::new(MockProvider::text("done")), 3)
            .with_verify(vec!["cargo check".into()], 1)
            .with_protocol_gates(gates)
            .with_session_label("sess-unverified")
            .with_diagnose_hook(hook);

        let mut stream = agent
            .run_stream(RunInput {
                prompt: "fix the bug".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        {
            let reports = captured.lock().unwrap();
            assert_eq!(reports.len(), 1, "unverified run must emit one report");
            assert_eq!(reports[0].outcome, "unverified");
            assert_eq!(reports[0].session_id, "sess-unverified");
        }

        // 对照：协议未启用 → 现状（suppress，无报告）。
        let captured2 = Arc::new(std::sync::Mutex::new(
            Vec::<crate::diagnose::DiagnoseReport>::new(),
        ));
        let cap2 = captured2.clone();
        let hook2: DiagnoseHook = Arc::new(move |report| cap2.lock().unwrap().push(report));
        let agent2 = Agent::new(Arc::new(MockProvider::text("done")), 3)
            .with_verify(vec!["cargo check".into()], 1)
            .with_diagnose_hook(hook2);
        let mut stream2 = agent2
            .run_stream(RunInput {
                prompt: "fix the bug".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while stream2.next().await.is_some() {}
        assert!(
            captured2.lock().unwrap().is_empty(),
            "protocol disabled must keep suppress-on-success behavior"
        );
    }

    // -----------------------------------------------------------------------
    // 协议阶段3：对抗审查触发条件（纯函数）
    // -----------------------------------------------------------------------

    #[test]
    fn adversarial_review_needed_blocks_on_blocking_finding() {
        let findings = vec![QualityFinding {
            rule: "no-commit-secret".into(),
            severity: FindingSeverity::Blocking,
            passed: false,
            evidence: "AKIA...".into(),
        }];
        assert!(adversarial_review_needed(&findings, &[]));
    }

    #[test]
    fn adversarial_review_needed_ignores_non_blocking_findings() {
        let findings = vec![QualityFinding {
            rule: "oversized-write".into(),
            severity: FindingSeverity::Warning,
            passed: false,
            evidence: "1024 bytes".into(),
        }];
        assert!(!adversarial_review_needed(&findings, &[]));
    }

    #[test]
    fn adversarial_review_needed_triggers_on_sensitive_tool_call() {
        let msg = Message {
            role: Role::Assistant,
            content: String::new(),
            name: None,
            tool_calls: Some(vec![ToolCall {
                id: "t1".into(),
                ty: "function".into(),
                function: FunctionCall {
                    name: "bash".into(),
                    arguments: r#"{"command":"chmod 777 /etc/hosts"}"#.into(),
                },
            }]),
            tool_call_id: None,
            reasoning_content: None,
        };
        assert!(adversarial_review_needed(&[], &[msg]));
    }

    #[test]
    fn adversarial_review_needed_skips_benign_session() {
        let msg = Message {
            role: Role::Assistant,
            content: String::new(),
            name: None,
            tool_calls: Some(vec![ToolCall {
                id: "t1".into(),
                ty: "function".into(),
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"src/main.rs"}"#.into(),
                },
            }]),
            tool_call_id: None,
            reasoning_content: None,
        };
        assert!(!adversarial_review_needed(&[], &[msg]));
    }

    /// Bugbot #9 负例：bash/shell 类无 SENSITIVE_MARKERS 命中 → 不触发
    /// （任意 bash 调用不得烧子代理 token）；write_file 写普通路径 → 不
    /// 触发、写安全边界路径（/etc/）→ 触发；delete_file 保持无条件敏感。
    #[test]
    fn adversarial_review_needed_marker_gating_on_bash_and_write() {
        let call = |name: &str, args: &str| Message {
            role: Role::Assistant,
            content: String::new(),
            name: None,
            tool_calls: Some(vec![ToolCall {
                id: "t1".into(),
                ty: "function".into(),
                function: FunctionCall {
                    name: name.into(),
                    arguments: args.into(),
                },
            }]),
            tool_call_id: None,
            reasoning_content: None,
        };
        // bash 无 marker → 不触发。
        assert!(!adversarial_review_needed(
            &[],
            &[call("bash", r#"{"command":"ls -la"}"#)]
        ));
        // bash 命中 marker（chmod /etc/）→ 触发。
        assert!(adversarial_review_needed(
            &[],
            &[call("bash", r#"{"command":"chmod 777 /etc/hosts"}"#)]
        ));
        // write_file 普通源码路径 → 不触发。
        assert!(!adversarial_review_needed(
            &[],
            &[call(
                "write_file",
                r#"{"path":"src/main.rs","content":"fn main() {}"}"#
            )]
        ));
        // write_file 命中路径 marker（/etc/）→ 触发。
        assert!(adversarial_review_needed(
            &[],
            &[call(
                "write_file",
                r#"{"path":"/etc/hosts","content":"127.0.0.1 x"}"#
            )]
        ));
        // delete_file 无条件敏感（不依赖 marker）。
        assert!(adversarial_review_needed(
            &[],
            &[call("delete_file", r#"{"path":"src/main.rs"}"#)]
        ));
    }

    /// Bugbot #10：loop 级「Blocking 违规确实拒绝工具」接线测试——Hard
    /// plan-before-execute 门（无计划文本 → Blocking）在 Execute transition
    /// 拒绝本轮全部工具：工具不执行（调用计数 0）、ToolResult 回填
    /// "blocked by protocol gate"（replay 不变量：不产生悬空 tool_calls）、
    /// 会话正常完成（模型收到回填结果后产出最终文本）。
    #[tokio::test]
    async fn protocol_blocking_violation_rejects_tool_execution() {
        let invoked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let inv = invoked.clone();
        struct CountingSpy {
            inv: Arc<std::sync::atomic::AtomicUsize>,
        }
        #[async_trait::async_trait]
        impl Tool for CountingSpy {
            fn schema(&self) -> ToolSchema {
                ToolSchema {
                    name: "spy".to_string(),
                    description: "counting spy".to_string(),
                    parameters: serde_json::json!({"type":"object","properties":{}}),
                }
            }
            fn read_only(&self) -> bool {
                true
            }
            async fn execute(&self, _ctx: &ToolContext, _args: &str) -> anyhow::Result<String> {
                self.inv.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok("ran".to_string())
            }
        }

        let provider = MockProvider::tool_call("spy", "{}", "ignored", "done");
        let levels = HashMap::from([(
            "plan-before-execute".to_string(),
            crate::phase_runner::GateLevel::Hard,
        )]);
        let gates = crate::phase_runner::builtin_phase_gates(&levels);
        let mut agent = Agent::new(Arc::new(provider), 5).with_protocol_gates(gates);
        agent.register_tool(Arc::new(CountingSpy { inv }));
        let events = drain(agent, "do the thing").await;

        // 门确实产出 Blocking 违规。
        assert!(
            events.iter().any(|e| matches!(
                e,
                RunEvent::GateViolation(v)
                    if v.gate == "plan-before-execute" && v.severity == FindingSeverity::Blocking
            )),
            "plan-before-execute Hard 门应在 Execute transition 产 Blocking"
        );
        // 工具结果回填 blocked 错误（模型可见，无悬空 tool_calls）。
        let blocked: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                RunEvent::ToolResult { result, .. } => Some(result.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            blocked
                .iter()
                .any(|r| r.contains("blocked by protocol gate")),
            "ToolResult 必须回填 'blocked by protocol gate'：{blocked:?}"
        );
        // 工具确实未被调用。
        assert_eq!(
            invoked.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "Blocking 违规时工具不得执行"
        );
        // 会话正常完成。
        assert!(events.iter().any(|e| matches!(e, RunEvent::Done(_))));
    }

    /// Bugbot #10/#5/#3：drift 语义 agent 级行为断言——bash 连续 4 败只产
    /// 1 条 DriftFinding（阈值首次跨越，窗口未清零前不重复发）、无 Blocking
    /// 违规、无工具被协议拒绝（#3：drift 二次不再走 Blocking/Ask，降级
    /// Warning + 需人工确认，见 phase_runner 单测）。
    #[tokio::test]
    async fn protocol_drift_emits_single_finding_and_never_blocks() {
        let tool_turn = |id: &str| {
            vec![
                Chunk::ToolCallStart {
                    id: id.to_string(),
                    name: "bash".to_string(),
                },
                Chunk::ToolCallEnd {
                    id: id.to_string(),
                    name: "bash".to_string(),
                    arguments: "{}".to_string(),
                },
                Chunk::Done,
            ]
        };
        let provider = MockProvider::sequential(vec![
            tool_turn("c1"),
            tool_turn("c2"),
            tool_turn("c3"),
            tool_turn("c4"),
            vec![
                Chunk::TextDelta("done".to_string()),
                Chunk::Usage(Usage::default()),
                Chunk::Done,
            ],
        ]);
        let mut agent = Agent::new(Arc::new(provider), 10)
            .with_protocol_gates(crate::phase_runner::builtin_phase_gates(&HashMap::new()));
        agent.register_tool(Arc::new(BashSpy { fail: true }));
        let events = drain(agent, "run bash").await;

        let drifts = events
            .iter()
            .filter(|e| matches!(e, RunEvent::DriftFinding(_)))
            .count();
        assert_eq!(
            drifts, 1,
            "连续 4 败只应发 1 条 DriftFinding（阈值首次跨越）：{events:?}"
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                RunEvent::GateViolation(v) if v.severity == FindingSeverity::Blocking
            )),
            "drift 路径不得产 Blocking（Bugbot #3：二次 drift 降级 Warning）：{events:?}"
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                RunEvent::ToolResult { result, .. } if result.contains("blocked by protocol gate")
            )),
            "drift 路径不得触发协议拒绝：{events:?}"
        );
        assert!(
            events.iter().any(|e| matches!(e, RunEvent::Done(_))),
            "会话应正常完成"
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

    #[test]
    fn default_system_prompt_defines_decision_engine_loop() {
        assert!(
            DEFAULT_SYSTEM_PROMPT.contains("decision engine"),
            "default prompt must encode the decision-engine principle"
        );
        for phase in [
            "Observe",
            "Plan",
            "Tool",
            "Verify",
            "Reflect",
            "Next Action",
        ] {
            assert!(
                DEFAULT_SYSTEM_PROMPT.contains(phase),
                "default prompt must define the {phase} phase"
            );
        }
    }

    #[tokio::test]
    async fn agent_injects_default_system_prompt_when_unconfigured() {
        let history = Arc::new(tokio::sync::Mutex::new(Vec::<Message>::new()));
        let agent = Agent::new(Arc::new(MockProvider::text("ok")), 3)
            .with_conversation_history(history.clone());

        let _ = drain(agent, "hello").await;

        let store = history.lock().await;
        let sys = store
            .iter()
            .find(|m| m.role == Role::System)
            .expect("default system prompt must be injected when unconfigured");
        assert_eq!(sys.content, DEFAULT_SYSTEM_PROMPT);
    }

    #[test]
    fn with_appended_on_none_prepends_default_system_prompt() {
        let agent = Agent::new(Arc::new(MockProvider::text("ok")), 3)
            .with_appended_system_prompt("EXTRA_HINT");
        let sp = agent
            .system_prompt
            .expect("appending to an unconfigured prompt must materialize one");
        assert!(sp.starts_with(DEFAULT_SYSTEM_PROMPT));
        assert!(sp.ends_with("EXTRA_HINT"));
    }

    #[tokio::test]
    async fn config_system_prompt_override_wins_over_default() {
        let history = Arc::new(tokio::sync::Mutex::new(Vec::<Message>::new()));
        let agent = Agent::new(Arc::new(MockProvider::text("ok")), 3)
            .with_system_prompt("CUSTOM_PROMPT")
            .with_conversation_history(history.clone());

        let _ = drain(agent, "hello").await;

        let store = history.lock().await;
        let sys = store
            .iter()
            .find(|m| m.role == Role::System)
            .expect("system message must exist");
        assert_eq!(sys.content, "CUSTOM_PROMPT");
        assert!(!sys.content.contains(DEFAULT_SYSTEM_PROMPT));
    }

    #[test]
    fn verify_failure_message_keeps_retry_contract() {
        let m = verify_failure_message("tests failed");
        assert!(m.contains("[verification failed]"));
        assert!(m.contains("tests failed"));
        assert!(m.contains("run again before completion"));
    }

    #[test]
    fn compression_prompt_preserves_facts_contract() {
        let p = render_compression_prompt("bash", "exit 1\nsecret=abc");
        assert!(p.contains("`bash`"));
        assert!(p.contains("exit 1\nsecret=abc"));
        assert!(p.contains("Preserve every fact"));
        assert!(p.contains("Output only the summary"));
        assert!(p.contains("Observe stage"));
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
    async fn post_edit_lsp_diagnostics_are_injected_into_tool_result() {
        // 编辑后诊断：write_file 成功 → 自动调用 lsp_diagnostics 并把结果
        // 拼进 ToolResult，模型下一轮就能看到编译/类型错误。
        let write = Arc::new(SpyTool {
            name: "write_file",
            result: "wrote ok".into(),
        });
        let lsp = Arc::new(SpyTool {
            name: "lsp_diagnostics",
            result: "LSP diagnostics for src/a.rs:\n- error: boom (line 1)".into(),
        });
        let responses = vec![
            vec![
                Chunk::ToolCallStart {
                    id: "call_1".into(),
                    name: "write_file".into(),
                },
                Chunk::ToolCallEnd {
                    id: "call_1".into(),
                    name: "write_file".into(),
                    arguments: r#"{"path":"src/a.rs"}"#.into(),
                },
                Chunk::Done,
            ],
            vec![
                Chunk::TextDelta("done after edit".into()),
                Chunk::Usage(Usage::default()),
                Chunk::Done,
            ],
        ];
        let write_trait = Arc::clone(&write) as Arc<dyn Tool>;
        let provider = Arc::new(MockProvider::sequential(responses).with_tools(vec![write_trait]));
        let mut agent = Agent::new(provider, 5);
        agent.register_tool(write);
        agent.register_tool(lsp);

        let mut stream = agent
            .run_stream(RunInput {
                prompt: "write a file".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        let mut injected = false;
        while let Some(event) = stream.next().await {
            if let Ok(RunEvent::ToolResult { result, .. }) = event {
                if result.contains("LSP diagnostics for src/a.rs") && result.contains("error: boom")
                {
                    injected = true;
                }
            }
        }
        assert!(
            injected,
            "post-edit LSP diagnostics must be injected into ToolResult"
        );
    }

    #[tokio::test]
    async fn auto_router_decides_once_per_run_and_routes_all_steps() {
        let spy = Arc::new(SpyTool {
            name: "spy",
            result: "tool executed!".into(),
        });
        let routed = Arc::new(
            MockProvider::sequential(vec![
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
                    Chunk::TextDelta("routed final".into()),
                    Chunk::Usage(Usage::default()),
                    Chunk::Done,
                ],
            ])
            .with_tools(vec![Arc::clone(&spy) as Arc<dyn Tool>]),
        );
        let decider_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let decider = Arc::new(FakeAutoDecider {
            calls: Arc::clone(&decider_calls),
            provider: Arc::clone(&routed) as Arc<dyn Provider>,
        });
        let mut agent =
            Agent::new(Arc::new(MockProvider::text("fallback")), 5).with_auto_router(decider);
        agent.register_tool(spy);

        let mut stream = agent
            .run_stream(RunInput {
                prompt: "use the tool".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        let mut saw_tool_result = false;
        let mut saw_routed_text = false;
        while let Some(event) = stream.next().await {
            match event.unwrap() {
                RunEvent::ToolResult { .. } => saw_tool_result = true,
                RunEvent::TextDelta(t) if t == "routed final" => saw_routed_text = true,
                _ => {}
            }
        }

        assert!(saw_tool_result, "routed provider's tool call must execute");
        assert!(saw_routed_text, "routed provider must drive the whole run");
        assert_eq!(routed.call_count(), 2, "two provider steps in one run");
        assert_eq!(
            decider_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "decision must happen once per run, not per step"
        );
    }

    #[tokio::test]
    async fn auto_router_decides_per_run_under_concurrency() {
        let routed = Arc::new(MockProvider::text("routed"));
        let decider_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let decider = Arc::new(FakeAutoDecider {
            calls: Arc::clone(&decider_calls),
            provider: Arc::clone(&routed) as Arc<dyn Provider>,
        });
        let agent = Arc::new(
            Agent::new(Arc::new(MockProvider::text("fallback")), 3).with_auto_router(decider),
        );

        let a1 = Arc::clone(&agent);
        let a2 = Arc::clone(&agent);
        let (r1, r2) = tokio::join!(
            async move {
                let mut s = a1
                    .run_stream(RunInput {
                        prompt: "one".into(),
                        images: vec![],
                        model_override: None,
                    })
                    .await
                    .unwrap();
                while s.next().await.is_some() {}
            },
            async move {
                let mut s = a2
                    .run_stream(RunInput {
                        prompt: "two".into(),
                        images: vec![],
                        model_override: None,
                    })
                    .await
                    .unwrap();
                while s.next().await.is_some() {}
            }
        );
        let _ = (r1, r2);
        assert_eq!(
            routed.call_count(),
            2,
            "each concurrent run gets its own routed provider use"
        );
        assert_eq!(
            decider_calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "each run decides independently (no shared cache)"
        );
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

    #[test]
    fn approval_risk_prefix_maps_readonly_kinds() {
        use deepseeknova_permission::{Decision, PermissionGate, Policy};

        let gate = PermissionGate::new(Policy {
            mode: Decision::Ask,
            allow: vec![],
            ask: vec![],
            deny: vec![],
        });
        assert_eq!(
            approval_risk_prefix(Some(&gate), "bash", r#"{"command": "git status"}"#).as_deref(),
            Some("[风险:只读]")
        );
        assert_eq!(
            approval_risk_prefix(Some(&gate), "Bash", r#"{"command": "rm -rf /tmp/x"}"#).as_deref(),
            Some("[风险:非只读]")
        );
        assert_eq!(
            approval_risk_prefix(
                Some(&gate),
                "shell",
                r#"{"command": "git -c core.pager='sh -x' status"}"#
            )
            .as_deref(),
            Some("[风险:危险]")
        );
        assert_eq!(
            approval_risk_prefix(Some(&gate), "grep", r#"{"command": "x"}"#),
            None
        );
        assert_eq!(
            approval_risk_prefix(None, "bash", r#"{"command": "x"}"#),
            None
        );
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
        // 新契约：阻断信息须解释原因（deny rule 命中）；规则拒绝不附
        // "添加 allow 即可放行"建议（deny 优先于 allow，该建议无效）
        assert!(
            tool_result.contains("blocked by deny rule"),
            "denied tool result should name the deny rule, got: {tool_result}"
        );
        assert!(
            !tool_result.contains("[建议]"),
            "rule denial should not carry an ineffective allow suggestion, got: {tool_result}"
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

    /// Ask-mode 门（无匹配规则 → 写工具一律 Ask）的构造助手。
    fn ask_mode_gate() -> Arc<PermissionGate> {
        use deepseeknova_permission::{Decision, PermissionGate, Policy};
        Arc::new(PermissionGate::new(Policy {
            mode: Decision::Ask,
            allow: vec![],
            ask: vec![],
            deny: vec![],
        }))
    }

    #[tokio::test]
    async fn ask_without_responder_fail_closed_by_default() {
        use std::sync::atomic::Ordering;

        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let provider = Arc::new(MockProvider::sequential(call_danger_then_done()));
        // 无审批 responder + 默认兜底（deny）：Ask 决策必须 fail-closed 拒绝，
        // 与子代理侧（sub_agent.rs）语义一致，不再静默放行写工具。
        let mut agent = Agent::new(provider, 5).with_permission_gate(ask_mode_gate());
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
            "Ask without responder must be denied (fail-closed)"
        );
        assert!(
            tool_result.contains("no approval responder"),
            "denial must explain the fail-closed reason, got: {tool_result}"
        );
    }

    #[tokio::test]
    async fn ask_without_responder_allow_opt_in_restores_auto_allow() {
        use std::sync::atomic::Ordering;

        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let provider = Arc::new(MockProvider::sequential(call_danger_then_done()));
        // 显式配置 ask_without_responder = "allow"：恢复旧的自动放行契约。
        let mut agent = Agent::new(provider, 5)
            .with_permission_gate(ask_mode_gate())
            .with_ask_without_responder_deny(false);
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
            "ask_without_responder = allow must auto-allow the Ask tool"
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

    #[tokio::test]
    async fn metrics_hook_fires_once_with_completed_snapshot() {
        use std::sync::Mutex as StdMutex;
        let fired = Arc::new(StdMutex::new(Vec::new()));
        let f2 = fired.clone();
        let hook: MetricsHook = Arc::new(move |stats, summary| {
            f2.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((stats, summary));
        });
        let agent = Agent::new(Arc::new(MockProvider::text("ok")), 3)
            .with_system_prompt("sp")
            .with_metrics_hook(hook);
        let mut stream = agent
            .run_stream(RunInput {
                prompt: "do it".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}
        // hook 由 spawned task 在流耗尽后触发，与 distill 同款有界等待。
        for _ in 0..50 {
            if !fired.lock().unwrap_or_else(|e| e.into_inner()).is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let snapshots = fired.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(snapshots.len(), 1, "metrics hook must fire exactly once");
        assert_eq!(snapshots[0].0.steps, 1);
        assert_eq!(snapshots[0].0.outcome, Some(RunOutcome::Completed));
        // 无 hook 链时 summary 全空（findings/reflection/review 均为默认值）。
        assert!(snapshots[0].1.findings.is_empty());
        assert_eq!(snapshots[0].1.reflection_count, 0);
        assert_eq!(snapshots[0].1.review_passes, 0);
        assert_eq!(snapshots[0].1.review_issues, 0);
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
    fn temp_git_repo_with_diff() -> (std::path::PathBuf, tempfile::TempDir) {
        // 用 tempfile::tempdir()：并行测试共用时间戳目录会撞名
        // （两个 "pause" 用例同时 git init → File exists）。
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_path_buf();
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
        (dir, tmp)
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

    /// 测试用 ToolHook：after 恒产出 Blocking finding。B3 review 短路门
    /// `wrote_files && quality_blocked` 要求 Blocking 级 finding 才进入
    /// review——08-04 的 review 测试未注册钩子，A 阶段接入短路后 review
    /// 被整体跳过（直接 Done），补此钩子恢复原测试语义。
    struct BlockingFindingHook;

    impl ToolHook for BlockingFindingHook {
        fn name(&self) -> &str {
            "blocking-finding-hook"
        }
        fn after(
            &self,
            _ctx: &ToolHookCtx,
            _call: &ToolCall,
            _result: &str,
        ) -> Vec<QualityFinding> {
            vec![QualityFinding {
                rule: "test-blocking".into(),
                severity: FindingSeverity::Blocking,
                passed: false,
                evidence: "test".into(),
            }]
        }
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
        let (repo, _tmp) = temp_git_repo_with_diff();
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
            .with_review_counter(hook)
            .with_tool_hook(Arc::new(BlockingFindingHook));
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
    }

    #[tokio::test]
    async fn review_persistent_issues_pauses() {
        let (repo, _tmp) = temp_git_repo_with_diff();
        // 审查 provider 永远回 issues（单响应重复模式）。
        let reviewer = Arc::new(SeqProvider::new(vec![
            r#"{"verdict":"issues","issues":["still broken"]}"#,
        ]));
        let provider = Arc::new(MockProvider::sequential(write_then_texts(&[
            "done v1", "done v2",
        ])));
        let mut agent = Agent::new(provider, 6)
            .with_workspace_root(repo.clone())
            .with_review(reviewer, 4000, 1)
            .with_tool_hook(Arc::new(BlockingFindingHook));
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
    }

    #[tokio::test]
    async fn review_pause_reason_includes_fix_plan_when_attribution_enabled() {
        // review 达 max_cycles → Paused 前归因；reason 附带 fix_plan（恢复建议，
        // Paused 恢复时可续用），与 verify 达上限路径共用 attribute_pause_reason。
        let (repo, _tmp) = temp_git_repo_with_diff();
        // 审查 provider 永远回 issues（单响应重复模式）。
        let reviewer = Arc::new(SeqProvider::new(vec![
            r#"{"verdict":"issues","issues":["still broken"]}"#,
        ]));
        let attrib = Arc::new(SeqProvider::new(vec![
            r#"{"root_cause":"broken import","verdict":"abort","fix_plan":"rewrite the import"}"#,
        ]));
        let provider = Arc::new(MockProvider::sequential(write_then_texts(&[
            "done v1", "done v2",
        ])));
        let mut agent = Agent::new(provider, 6)
            .with_workspace_root(repo.clone())
            .with_review(reviewer, 4000, 1)
            .with_attribution(crate::attribution::AttributionSettings {
                provider: attrib.clone(),
                max_retries: 0,
                max_attributions: 3,
                degrade_map: std::collections::HashMap::new(),
            })
            .with_tool_hook(Arc::new(BlockingFindingHook));
        agent.register_tool(Arc::new(SpyTool {
            name: "write_file",
            result: "written".into(),
        }));

        let events = drain(agent, "write something").await;
        let reason = events
            .iter()
            .find_map(|e| match e {
                RunEvent::Paused { reason, .. } => Some(reason.clone()),
                _ => None,
            })
            .expect("must emit Paused on persistent review issues");
        assert!(reason.starts_with("review_issues:"), "got: {reason}");
        assert!(
            reason.contains("fix_plan: rewrite the import"),
            "Paused reason must carry fix_plan, got: {reason}"
        );
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
    async fn verify_pause_reason_includes_fix_plan_when_attribution_enabled() {
        // verify 达 max_cycles → Paused 前归因；reason 附带 fix_plan（恢复建议，
        // Paused 恢复时可续用）。归因失败/关闭时 reason 保持原文案。
        let provider = Arc::new(MockProvider::sequential(write_then_texts(&["done v1"])));
        let attrib = Arc::new(SeqProvider::new(vec![
            r#"{"root_cause":"broken import","verdict":"abort","fix_plan":"rewrite the import"}"#,
        ]));
        let mut agent = Agent::new(provider, 6)
            .with_verify(vec!["cargo check --quiet".into()], 1)
            .with_attribution(crate::attribution::AttributionSettings {
                provider: attrib.clone(),
                max_retries: 0,
                max_attributions: 3,
                degrade_map: std::collections::HashMap::new(),
            });
        agent.register_tool(Arc::new(WritableSpy {
            name: "write_file",
            result: "written".into(),
        }));
        agent.register_tool(Arc::new(BashSpy { fail: true }));

        let events = drain(agent, "write something").await;
        let reason = events
            .iter()
            .find_map(|e| match e {
                RunEvent::Paused { reason, .. } => Some(reason.clone()),
                _ => None,
            })
            .expect("must emit Paused on persistent verify failure");
        assert!(reason.starts_with("verify_failed:"), "got: {reason}");
        assert!(
            reason.contains("fix_plan: rewrite the import"),
            "Paused reason must carry fix_plan, got: {reason}"
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

    // -----------------------------------------------------------------------
    // F3：interested() panic 按未注册处理（不 panic run，钩子不执行）
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn interested_panic_treated_as_not_registered() {
        struct InterestedPanicHook;
        impl ToolHook for InterestedPanicHook {
            fn name(&self) -> &str {
                "interested-panic"
            }
            fn interested(&self, _call: &ToolCall) -> bool {
                panic!("interested panic")
            }
            fn before(&self, _ctx: &ToolHookCtx, _call: &ToolCall) -> HookVerdict {
                HookVerdict::Deny("should never run".into())
            }
            fn after(
                &self,
                _ctx: &ToolHookCtx,
                _call: &ToolCall,
                _result: &str,
            ) -> Vec<QualityFinding> {
                vec![QualityFinding {
                    rule: "never".into(),
                    severity: FindingSeverity::Blocking,
                    passed: false,
                    evidence: "never".into(),
                }]
            }
        }

        let workspace = std::env::temp_dir().join(format!(
            "dnv-interested-panic-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        let provider = MockProvider::tool_call(
            "write_file",
            r#"{"path":"src/main.rs","content":"fn main() {}"}"#,
            "ignored",
            "done",
        );
        let mut agent = Agent::new(Arc::new(provider), 5)
            .with_workspace_root(workspace.clone())
            .with_tool_hook(Arc::new(InterestedPanicHook));
        agent.register_tool(Arc::new(WritableSpy {
            name: "write_file",
            result: "written".into(),
        }));
        let events = drain(agent, "write main.rs").await;
        // interested panic → 按未注册处理：before/after 均未执行，工具正常跑。
        assert!(events.iter().any(|e| matches!(e, RunEvent::Done(_))));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, RunEvent::ToolResult { .. })),
            "tool must execute and emit ToolResult (hook treated as not registered)"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, RunEvent::QualityFinding(_))),
            "panicking interested must not produce findings"
        );
        let _ = std::fs::remove_dir_all(&workspace);
    }

    // -----------------------------------------------------------------------
    // F4：MetricsGuard run 级差分切片（run 前已有历史 findings 不混入）
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn metrics_summary_findings_are_run_scoped() {
        use std::sync::Mutex as StdMutex;
        let fired = Arc::new(StdMutex::new(Vec::<QualitySummary>::new()));
        let f2 = fired.clone();
        let hook: MetricsHook = Arc::new(move |_stats, summary| {
            f2.lock().unwrap_or_else(|e| e.into_inner()).push(summary);
        });
        // 同一 agent 跑两次 run：quality_findings 跨 run 累积，summary 只含本 run。
        let mut agent = Agent::new(
            Arc::new(MockProvider::sequential(vec![
                // run 1：write_file 工具调用 + 完成文本。
                vec![
                    Chunk::ToolCallStart {
                        id: "w1".into(),
                        name: "write_file".into(),
                    },
                    Chunk::ToolCallEnd {
                        id: "w1".into(),
                        name: "write_file".into(),
                        arguments: r#"{"path":"src/a.rs","content":"x"}"#.into(),
                    },
                    Chunk::Done,
                ],
                vec![
                    Chunk::TextDelta("done1".into()),
                    Chunk::Usage(Usage::default()),
                    Chunk::Done,
                ],
                // run 2：同上。
                vec![
                    Chunk::ToolCallStart {
                        id: "w2".into(),
                        name: "write_file".into(),
                    },
                    Chunk::ToolCallEnd {
                        id: "w2".into(),
                        name: "write_file".into(),
                        arguments: r#"{"path":"src/b.rs","content":"y"}"#.into(),
                    },
                    Chunk::Done,
                ],
                vec![
                    Chunk::TextDelta("done2".into()),
                    Chunk::Usage(Usage::default()),
                    Chunk::Done,
                ],
            ])),
            5,
        )
        .with_metrics_hook(hook)
        .with_tool_hook(Arc::new(BlockingFindingHook));
        agent.register_tool(Arc::new(WritableSpy {
            name: "write_file",
            result: "written".into(),
        }));

        // 同一 agent 跑两次 run（run_stream 取 &self，可复用）。
        for _ in 0..2 {
            let mut stream = agent
                .run_stream(RunInput {
                    prompt: "write a file".into(),
                    images: vec![],
                    model_override: None,
                })
                .await
                .unwrap();
            while let Some(ev) = stream.next().await {
                let _ = ev.unwrap();
            }
            for _ in 0..50 {
                if fired.lock().unwrap_or_else(|e| e.into_inner()).len() >= 2 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }
        let summaries = fired.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(summaries.len(), 2, "two runs must fire two summaries");
        // 每个 run 的 summary 只含本 run 新增的 1 条 finding（run 级差分切片），
        // 而非会话累计的 1 条 / 2 条。
        assert_eq!(
            summaries[0].findings.len(),
            1,
            "run 1 summary must contain exactly its own finding, got {}",
            summaries[0].findings.len()
        );
        assert_eq!(
            summaries[1].findings.len(),
            1,
            "run 2 summary must contain exactly its own finding (no cross-run pollution), got {}",
            summaries[1].findings.len()
        );
    }

    #[test]
    fn unique_run_label_is_unique_and_serve_safe() {
        let a = unique_run_label();
        let b = unique_run_label();
        assert_ne!(a, b, "每次 run 必须拿到唯一标注");
        for label in [&a, &b] {
            assert!(label.starts_with("session-"), "unexpected label: {label}");
            assert!(
                label
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "label must only contain [A-Za-z0-9_-]: {label}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // F4：DiagnoseGuard / Paused session_id 同样按 run 切片 + 每次 run 唯一
    // -----------------------------------------------------------------------

    /// 同一 agent 连续两次 Paused run：诊断报告的 quality 只含本 run 新增的
    /// findings（不被历史 run 污染），且未标注时的 session_id 每次 run 唯一。
    #[tokio::test]
    async fn diagnose_reports_are_run_scoped_with_unique_session_ids() {
        use crate::diagnose::DiagnoseReport;
        let captured = Arc::new(std::sync::Mutex::new(Vec::<DiagnoseReport>::new()));
        let cap = captured.clone();
        let hook: DiagnoseHook = Arc::new(move |report| {
            cap.lock().unwrap_or_else(|e| e.into_inner()).push(report);
        });
        let agent = looping_agent(2)
            .with_on_max_steps("pause")
            .with_tool_hook(Arc::new(BlockingFindingHook))
            .with_diagnose_hook(hook);

        for _ in 0..2 {
            let mut stream = agent
                .run_stream(RunInput {
                    prompt: "write a file".into(),
                    images: vec![],
                    model_override: None,
                })
                .await
                .unwrap();
            while let Some(ev) = stream.next().await {
                let _ = ev.unwrap();
            }
            for _ in 0..50 {
                if captured.lock().unwrap_or_else(|e| e.into_inner()).len() >= 2 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }

        let reports = captured.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(reports.len(), 2, "two runs must emit two diagnose reports");
        // 每个 run 的 max-steps=2 → 每轮工具调用产 1 条 Blocking finding，
        // 报告只应含本 run 的 2 条，而非会话累计的 2/4 条。
        for (i, report) in reports.iter().enumerate() {
            assert_eq!(
                report.quality.len(),
                2,
                "report {i} must contain exactly its own findings, got {}",
                report.quality.len()
            );
        }
        // 未显式标注：session_id 每次 run 唯一且 serve 路径安全。
        assert_ne!(reports[0].session_id, reports[1].session_id);
        for report in reports.iter() {
            assert!(
                report
                    .session_id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "report session_id must be serve-path safe: {}",
                report.session_id
            );
        }
    }

    // -----------------------------------------------------------------------
    // F9：quality findings 有界累积（超限丢弃新 finding）
    // -----------------------------------------------------------------------

    /// after 恒产出 N 条 finding 的钩子。
    struct FloodFindingHook {
        count: usize,
    }

    impl ToolHook for FloodFindingHook {
        fn name(&self) -> &str {
            "flood-finding-hook"
        }
        fn after(
            &self,
            _ctx: &ToolHookCtx,
            _call: &ToolCall,
            _result: &str,
        ) -> Vec<QualityFinding> {
            (0..self.count)
                .map(|i| QualityFinding {
                    rule: format!("flood-{i}"),
                    severity: FindingSeverity::Warning,
                    passed: false,
                    evidence: "flood".into(),
                })
                .collect()
        }
    }

    #[tokio::test]
    async fn quality_findings_capped_at_max() {
        let workspace = std::env::temp_dir().join(format!(
            "dnv-quality-cap-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        let provider = MockProvider::tool_call(
            "write_file",
            r#"{"path":"src/a.rs","content":"x"}"#,
            "ignored",
            "done",
        );
        let agent = Agent::new(Arc::new(provider), 5)
            .with_workspace_root(workspace.clone())
            .with_tool_hook(Arc::new(FloodFindingHook {
                count: MAX_QUALITY_FINDINGS + 1,
            }));
        let mut agent = agent;
        agent.register_tool(Arc::new(WritableSpy {
            name: "write_file",
            result: "written".into(),
        }));
        let events = drain(agent, "flood findings").await;
        // 事件流照常全部发出（用户可见）。
        let emitted = events
            .iter()
            .filter(|e| matches!(e, RunEvent::QualityFinding(_)))
            .count();
        assert_eq!(
            emitted,
            MAX_QUALITY_FINDINGS + 1,
            "all findings must reach the event stream"
        );
        // 会话累计被截断到上限——通过 metrics summary 快照断言（findings
        // 切片 [start_len..] = 会话新增 = min(上限, 产出数)）。
        let fired = Arc::new(std::sync::Mutex::new(Vec::<QualitySummary>::new()));
        let f2 = fired.clone();
        let hook: MetricsHook = Arc::new(move |_stats, summary| {
            f2.lock().unwrap_or_else(|e| e.into_inner()).push(summary);
        });
        // 重跑：带 metrics hook 断言存储截断。
        let provider = MockProvider::tool_call(
            "write_file",
            r#"{"path":"src/a.rs","content":"x"}"#,
            "ignored",
            "done",
        );
        let mut agent2 = Agent::new(Arc::new(provider), 5)
            .with_workspace_root(workspace.clone())
            .with_metrics_hook(hook)
            .with_tool_hook(Arc::new(FloodFindingHook {
                count: MAX_QUALITY_FINDINGS + 1,
            }));
        agent2.register_tool(Arc::new(WritableSpy {
            name: "write_file",
            result: "written".into(),
        }));
        let _ = drain(agent2, "flood findings again").await;
        for _ in 0..50 {
            if !fired.lock().unwrap_or_else(|e| e.into_inner()).is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let summaries = fired.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            summaries[0].findings.len(),
            MAX_QUALITY_FINDINGS,
            "stored findings must be capped at MAX_QUALITY_FINDINGS"
        );
        let _ = std::fs::remove_dir_all(&workspace);
    }

    // -----------------------------------------------------------------------
    // F5：取消路径 suppress 诊断（不产报告）
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn cancel_path_suppresses_diagnose_report() {
        use crate::diagnose::DiagnoseReport;
        let captured = Arc::new(std::sync::Mutex::new(Vec::<DiagnoseReport>::new()));
        let cap = captured.clone();
        let hook: DiagnoseHook = Arc::new(move |report| cap.lock().unwrap().push(report));
        let (tx, mut rx) = mpsc::channel(64);
        let memory = Arc::new(tokio::sync::RwLock::new(Memory::new()));
        let cancel = CancellationToken::new();
        cancel.cancel(); // 预置取消：步边界立即走取消路径
        let provider = Arc::new(MockProvider::text("never used"));
        let quality_findings = Arc::new(tokio::sync::Mutex::new(Vec::<QualityFinding>::new()));
        let result = run_agent_loop(
            provider,
            Vec::new(),
            5,
            None,
            memory.clone(),
            RunInput {
                prompt: "x".into(),
                images: vec![],
                model_override: None,
            },
            &tx,
            &cancel,
            std::env::temp_dir(),
            SecurityContext::with_safe_defaults(),
            None,
            None,
            true, // ask_without_responder_deny：无 responder 默认 fail-closed
            Vec::new(),
            true,
            true,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            &[],
            &UserHooks::default(),
            &quality_findings,
            Some(hook),
            Vec::new(), // 协议门控（阶段3）：空 = 关闭
            false,      // 对抗审查（阶段3）：关闭
        )
        .await;
        assert!(result.is_ok(), "cancel path must return Ok");
        // Done 事件已发出。
        while let Some(ev) = rx.recv().await {
            if matches!(ev, Ok(RunEvent::Done(_))) {
                break;
            }
        }
        drop(rx);
        // 取消 → suppress → 诊断 hook 不被调用（不落盘，无 outcome=failed 误报）。
        assert!(
            captured.lock().unwrap().is_empty(),
            "cancel must not emit a diagnose report"
        );
    }

    // -----------------------------------------------------------------------
    // F10：无写文件场景不产出空 verify 相位
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn verify_phase_absent_without_file_writes() {
        use crate::diagnose::DiagnoseReport;
        let captured = Arc::new(std::sync::Mutex::new(Vec::<DiagnoseReport>::new()));
        let cap = captured.clone();
        let hook: DiagnoseHook = Arc::new(move |report| cap.lock().unwrap().push(report));
        // max-steps pause 路径产出报告；配置 verify（有命令）但 run 从不写文件
        // （只读工具）→ 报告 phases 不含 verify。
        let agent = looping_agent(2)
            .with_on_max_steps("pause")
            .with_diagnose_hook(hook)
            .with_verify(vec!["cargo check".into()], 2);
        let mut stream = agent
            .run_stream(RunInput {
                prompt: "loop forever".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}
        let reports = captured.lock().unwrap();
        assert_eq!(reports.len(), 1, "max-steps pause must emit one report");
        let names: Vec<&str> = reports[0].phases.iter().map(|p| p.name.as_str()).collect();
        assert!(
            !names.contains(&"verify"),
            "no empty verify phase expected without file writes, got {names:?}"
        );
        assert!(names.contains(&"plan"), "plan phase must be present");
    }

    // -----------------------------------------------------------------------
    // 用户级外部 hooks（`[hooks]` 段装配而来）
    // -----------------------------------------------------------------------

    /// 构造一条「追加固定文本到文件」的外部命令（`sh -c "echo .. >> file"`），
    /// 用作外部命令 mock：通过文件内容断言钩子是否触发。
    fn marker_hook(marker: &str, path: &std::path::Path) -> UserHookCommand {
        UserHookCommand {
            command: "sh".into(),
            args: vec![
                "-c".into(),
                format!("echo '{}' >> '{}'", marker, path.display()),
            ],
            timeout: Some(std::time::Duration::from_secs(10)),
        }
    }

    /// 读取 marker 文件全部文本（不存在返回空串）。
    fn marker_text(path: &std::path::Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    /// 收集事件流中的工具结果文本。
    fn tool_results(events: &[RunEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                RunEvent::ToolResult { result, .. } => Some(result.clone()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn session_start_and_end_hooks_fire() {
        let tmp = tempfile::tempdir().unwrap();
        let markers = tmp.path().join("session.log");
        let hooks = UserHooks {
            session_start: vec![marker_hook("start", &markers)],
            session_end: vec![marker_hook("end", &markers)],
            ..Default::default()
        };
        let agent = Agent::new(Arc::new(MockProvider::text("done")), 3).with_user_hooks(hooks);
        let events = drain(agent, "hello").await;
        assert!(events.iter().any(|e| matches!(e, RunEvent::Done(_))));
        let text = marker_text(&markers);
        assert!(text.contains("start"), "session_start 必须触发: {text:?}");
        assert!(text.contains("end"), "session_end 必须触发: {text:?}");
        assert!(
            text.find("start") < text.find("end"),
            "session_start 应先于 session_end: {text:?}"
        );
    }

    #[tokio::test]
    async fn session_hooks_empty_means_no_process() {
        // 空 hooks：跑一轮成功 run 不触发任何外部命令（零开销路径不崩）。
        let agent = Agent::new(Arc::new(MockProvider::text("done")), 3)
            .with_user_hooks(UserHooks::default());
        let events = drain(agent, "hello").await;
        assert!(events.iter().any(|e| matches!(e, RunEvent::Done(_))));
    }

    #[tokio::test]
    async fn tool_before_allows_on_exit_zero() {
        let mut agent = Agent::new(
            Arc::new(MockProvider::tool_call("write_file", "{}", "ok", "done")),
            5,
        )
        .with_user_hooks(UserHooks {
            tool_before: vec![UserHookCommand {
                command: "true".into(),
                args: vec![],
                timeout: None,
            }],
            ..Default::default()
        });
        agent.register_tool(Arc::new(SpyTool {
            name: "write_file",
            result: "written".into(),
        }));
        let results = tool_results(&drain(agent, "write").await);
        assert!(
            results.iter().any(|r| r.contains("written")),
            "exit 0 的 tool_before 应放行，工具结果: {results:?}"
        );
    }

    #[tokio::test]
    async fn tool_before_blocks_on_nonzero_exit() {
        let mut agent = Agent::new(
            Arc::new(MockProvider::tool_call("write_file", "{}", "ok", "done")),
            5,
        )
        .with_user_hooks(UserHooks {
            tool_before: vec![UserHookCommand {
                command: "false".into(),
                args: vec![],
                timeout: None,
            }],
            ..Default::default()
        });
        agent.register_tool(Arc::new(SpyTool {
            name: "write_file",
            result: "written".into(),
        }));
        let results = tool_results(&drain(agent, "write").await);
        assert!(
            results.iter().any(|r| r.contains("blocked by user hook")),
            "非 0 退出的 tool_before 必须阻止执行（fail-closed），工具结果: {results:?}"
        );
        assert!(
            results.iter().all(|r| !r.contains("written")),
            "被阻止后工具不得执行"
        );
    }

    #[tokio::test]
    async fn tool_before_blocks_on_verdict_deny() {
        let mut agent = Agent::new(
            Arc::new(MockProvider::tool_call("write_file", "{}", "ok", "done")),
            5,
        )
        .with_user_hooks(UserHooks {
            tool_before: vec![UserHookCommand {
                command: "sh".into(),
                args: vec![
                    "-c".into(),
                    "echo '{\"allowed\":false,\"reason\":\"no writes\"}'".into(),
                ],
                timeout: None,
            }],
            ..Default::default()
        });
        agent.register_tool(Arc::new(SpyTool {
            name: "write_file",
            result: "written".into(),
        }));
        let results = tool_results(&drain(agent, "write").await);
        assert!(
            results.iter().any(|r| r.contains("no writes")),
            "裁决 allowed=false 必须阻止执行，工具结果: {results:?}"
        );
        assert!(
            results.iter().all(|r| !r.contains("written")),
            "裁决拒绝后工具不得执行"
        );
    }

    #[tokio::test]
    async fn tool_before_timeout_is_deny() {
        let mut agent = Agent::new(
            Arc::new(MockProvider::tool_call("write_file", "{}", "ok", "done")),
            5,
        )
        .with_user_hooks(UserHooks {
            tool_before: vec![UserHookCommand {
                command: "sleep".into(),
                args: vec!["5".into()],
                timeout: Some(std::time::Duration::from_millis(100)),
            }],
            ..Default::default()
        });
        agent.register_tool(Arc::new(SpyTool {
            name: "write_file",
            result: "written".into(),
        }));
        let results = tool_results(&drain(agent, "write").await);
        assert!(
            results.iter().any(|r| r.contains("blocked by user hook")),
            "超时的 tool_before 必须 fail-closed 拒绝，工具结果: {results:?}"
        );
    }

    #[tokio::test]
    async fn tool_after_failure_does_not_block() {
        let mut agent = Agent::new(
            Arc::new(MockProvider::tool_call("write_file", "{}", "ok", "done")),
            5,
        )
        .with_user_hooks(UserHooks {
            tool_after: vec![UserHookCommand {
                command: "false".into(),
                args: vec![],
                timeout: None,
            }],
            ..Default::default()
        });
        agent.register_tool(Arc::new(SpyTool {
            name: "write_file",
            result: "written".into(),
        }));
        let results = tool_results(&drain(agent, "write").await);
        assert!(
            results.iter().any(|r| r.contains("written")),
            "tool_after 失败仅 warn，不得阻止已执行工具: {results:?}"
        );
    }

    #[tokio::test]
    async fn failure_hook_fires_on_max_steps_paused() {
        let tmp = tempfile::tempdir().unwrap();
        let markers = tmp.path().join("failure.log");
        let hooks = UserHooks {
            failure: vec![marker_hook("failed", &markers)],
            ..Default::default()
        };
        let agent = looping_agent(2).with_user_hooks(hooks);
        let events = drain(agent, "loop").await;
        assert!(events.iter().any(|e| matches!(e, RunEvent::Paused { .. })));
        assert!(
            marker_text(&markers).contains("failed"),
            "max-steps Paused 必须触发 failure 事件（失败诊断时）"
        );
    }

    #[tokio::test]
    async fn failure_hook_not_fired_on_completed() {
        let tmp = tempfile::tempdir().unwrap();
        let markers = tmp.path().join("failure.log");
        let hooks = UserHooks {
            failure: vec![marker_hook("failed", &markers)],
            ..Default::default()
        };
        let agent = Agent::new(Arc::new(MockProvider::text("done")), 3).with_user_hooks(hooks);
        let events = drain(agent, "hello").await;
        assert!(events.iter().any(|e| matches!(e, RunEvent::Done(_))));
        assert!(
            marker_text(&markers).is_empty(),
            "成功完成不得触发 failure 事件"
        );
    }
}
