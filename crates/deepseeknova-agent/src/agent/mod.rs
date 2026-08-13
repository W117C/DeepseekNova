use crate::approval::{approval_risk_prefix, render_suggestions};
use crate::classify::{
    classify_quick_step, group_call_indices, history_last_turn_used_tools, unique_run_label,
};
use crate::diagnose::{DiagnoseGuard, DiagnoseHook};
use crate::memory::Memory;
use crate::path::{extract_tool_path, extract_touched_paths, tool_cache_key};
use crate::prompts::DEFAULT_SYSTEM_PROMPT;
use crate::recursion::DelegateDepth;
use crate::render::{
    render_adversarial_evidence, render_compression_prompt, verify_failure_message,
};
use crate::tools::inject_recall;
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
use deepseeknova_permission::{CheckVerdict, Decision, PermissionGate};
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

// M7 拆分：对抗审查触发判定实现已迁至 agent_diag.rs，此处 re-export 保持
// `deepseeknova_agent::adversarial_review_needed` 既有公开 API 不变。
pub use crate::agent_diag::adversarial_review_needed;

// ---------------------------------------------------------------------------
// Agent — the main agent runner
// ---------------------------------------------------------------------------

/// The main agent runner: multi-step reasoning loop with tool use, memory
/// management, streaming output, and cancellation support.
///
/// Build via [`Self::new`] plus the `with_*` builder methods, then hand it to
/// the runtime or drive it directly through the [`Runner`] trait.
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
    /// P2-A：轮末大工具结果缩容阈值（tokens）；None = 不启用（Reasonix 借鉴）。
    turn_end_result_cap_tokens: Option<u32>,
    /// P2-B：上下文占用达到预算的该比例时提前预防性缩容；None = 关闭。
    preventive_shrink_ratio: Option<f32>,

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

    /// 会话级 USD 花费上限（团队级花费上限，P2-4）；None = 关闭。与
    /// `budget` 并列：两者任一触发即停（先到先停）。由运行时从共享
    /// `ModelRouter` 的 ledger + price table 装配。
    cost_budget: Option<crate::budget::cost::CostBudget>,

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

    /// A4：写后 diff 规模审计开关（git 仓库内，变更行数超阈值告警注入
    /// ToolResult）；默认关闭（零配置行为不变）。
    diff_audit: bool,
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
    /// 本 run findings 的 Arc 引用（emit 时快照进 QualitySummary）。T11 起
    /// 主循环传入**本 run 局部暂存区**（`run_findings`），并发 run 各归各
    /// run；仍保留差分切片语义以兼容直接构造（测试/库级）传入会话容器。
    quality_findings: Option<Arc<tokio::sync::Mutex<Vec<QualityFinding>>>>,
    /// F4：起始长度基准。emit 时只取 `[start_len..]` 差分切片——传入
    /// run 局部暂存区时恒为 0（只含本 run）。`None` = 构造时锁被占用，
    /// 起始基准未知（此时 emit 一律报空 findings，绝不回退到 `0` 把并发
    /// run 的 findings 误切进本 run）。
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

    /// 记录一次会话内只读工具结果缓存命中（`[cached]` 短路）。
    fn observe_tool_cache_hit(&mut self) {
        self.tracker.observe_tool_cache_hit();
    }

    /// 记录一次会话内只读工具结果缓存未命中（实际执行并写入缓存）。
    fn observe_tool_cache_miss(&mut self) {
        self.tracker.observe_tool_cache_miss();
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
    /// Create an agent with a provider and a maximum number of steps
    /// (`0` falls back to 10).
    pub fn new(provider: Arc<dyn Provider>, max_steps: usize) -> Self {
        Self {
            provider,
            tools: HashMap::new(),
            max_steps: if max_steps == 0 { 10 } else { max_steps },
            system_prompt: None,
            workspace_root: std::env::current_dir().unwrap_or_default(),
            security: SecurityContext::with_safe_defaults(),

            compaction_threshold_tokens: None,
            turn_end_result_cap_tokens: None,
            preventive_shrink_ratio: None,
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
            cost_budget: None,
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
            diff_audit: false,
        }
    }

    /// Override the default system prompt (replaces the built-in contract).
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

    /// Override the token threshold that triggers conversation compaction;
    /// `None` disables the check.
    pub fn with_compaction_threshold(mut self, tokens: Option<u32>) -> Self {
        self.compaction_threshold_tokens = tokens;
        self
    }

    /// P2-A（Reasonix 借鉴）：启用轮末大工具结果缩容——每轮末对超过
    /// `cap`（tokens）的工具结果缩容，当轮看全文、后续看摘要、可按
    /// call_id 经 `fetch_full_result` 重读；降低每轮新增占比以提升前缀
    /// 缓存命中率。
    pub fn with_turn_end_result_cap(mut self, cap: u32) -> Self {
        self.turn_end_result_cap_tokens = Some(cap);
        self
    }

    /// P2-B（Reasonix 借鉴）：启用预防性缩容——上下文占用达到预算的
    /// `ratio`（0.0..=1.0，建议 0.4）比例时提前对大工具结果缩容，避免
    /// 80% 紧急阈值一次性大改缓存前缀。
    pub fn with_preventive_shrink_ratio(mut self, ratio: f32) -> Self {
        self.preventive_shrink_ratio = Some(ratio);
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

    /// 启用会话级 USD 花费上限（团队级花费上限，P2-4）。与 token 预算并列，
    /// 任一触发即停；由运行时从共享 `ModelRouter` 装配（`CostBudget::from_router`）。
    pub fn with_cost_budget(mut self, cb: crate::budget::cost::CostBudget) -> Self {
        self.cost_budget = Some(cb);
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
    /// 钩子，before/after 按注册顺序串行执行；钩子 panic 时按 fail-closed
    /// 处理（before panic → Deny 拒绝执行；after panic → 空 findings 不阻断）。
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

    /// A4：开启写后 diff 规模审计（git 仓库内，变更行数超阈值告警注入
    /// ToolResult）。默认关闭。
    pub fn with_diff_audit(mut self, enabled: bool) -> Self {
        self.diff_audit = enabled;
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

    /// Register a tool by its schema name. A later registration with the same
    /// name replaces the earlier tool.
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
/// injection set (workspace + security + root delegate depth + registered
/// extensions) in one place.
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
    // Agent 主循环不嵌套，恒为根深度 1：注入当前委派深度扩展，供
    // delegate 类工具读取后按 depth+1 派发（递归守门见 DelegateEngine /
    // RecursiveDelegateTool）。注入放在构建期扩展之前，运行时/测试可经
    // `with_extension` 覆盖为子代理的真实深度。
    ctx.extensions.insert(DelegateDepth(1));
    for apply in extensions {
        apply(&mut ctx.extensions);
    }
    ctx
}

#[async_trait::async_trait]
impl Runner for Agent {
    async fn run_stream(
        &self,
        input: RunInput,
    ) -> Result<RunEventStream, deepseeknova_core::DeepseeknovaError> {
        self.run_stream_with_extensions(input, Vec::new()).await
    }
}

impl Agent {
    /// 内部扩展注入版 run：除构建期扩展外，本次运行临时注入额外扩展
    /// （如子代理递归深度 `DelegateDepth`），不改变静态扩展集合。
    pub(crate) async fn run_stream_with_extensions(
        &self,
        input: RunInput,
        extra_extensions: Vec<Arc<ExtensionApplier>>,
    ) -> Result<RunEventStream, deepseeknova_core::DeepseeknovaError> {
        self.run_stream_with_parent_cancel(input, extra_extensions, None)
            .await
    }

    /// T12 接线：带父取消令牌的运行版本。`parent_cancel` 为父 run 的
    /// [`CancellationToken`]（主 agent 的 delegate 工具把
    /// [`deepseeknova_core::tool::ToolContext::cancellation`] 传入子代理执行）；
    /// 子代理使用其 `child_token()`，父取消后子代理在步边界/工具执行中途
    /// （`tokio::select!`）立即中止。`None` = 顶层运行：自建令牌并接线
    /// Ctrl-C（与既有行为一致）。
    pub(crate) async fn run_stream_with_parent_cancel(
        &self,
        input: RunInput,
        extra_extensions: Vec<Arc<ExtensionApplier>>,
        parent_cancel: Option<CancellationToken>,
    ) -> Result<RunEventStream, deepseeknova_core::DeepseeknovaError> {
        let (tx, rx) = mpsc::channel(64);

        let provider = Arc::clone(&self.provider);
        let tools: Vec<Arc<dyn Tool>> = self.tools.values().cloned().collect();
        let max_steps = self.max_steps;
        let system_prompt = self
            .system_prompt
            .clone()
            .unwrap_or_else(|| DEFAULT_SYSTEM_PROMPT.to_string());
        let compaction_threshold = self.compaction_threshold_tokens;
        let turn_end_result_cap = self.turn_end_result_cap_tokens;
        let preventive_shrink_ratio = self.preventive_shrink_ratio;
        let workspace_root = self.workspace_root.clone();
        let security = self.security.clone();
        let history = self.history.clone();
        let permission = self.permission.clone();
        let approval = self.approval.clone();
        let ask_without_responder_deny = self.ask_without_responder_deny;
        let extensions: Vec<Arc<ExtensionApplier>> = self
            .extensions
            .iter()
            .cloned()
            .chain(extra_extensions)
            .collect();
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
        // CostBudget 实现 Clone（内部 Arc + PriceTable）：直接 clone 带进 spawn。
        let cost_budget = self.cost_budget.clone();
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
        // 带父取消令牌时（子代理运行）：复用父令牌的子令牌，父取消即中止；
        // 不重复接线 Ctrl-C（顶层 run 已处理）。
        let cancel = match &parent_cancel {
            Some(parent) => parent.child_token(),
            None => {
                let c = CancellationToken::new();
                let cancel_clone = c.clone();
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
                c
            }
        };

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
        // A4：写后 diff 审计开关复制为局部值（spawn 内借用 self 会逃逸）。
        let diff_audit = self.diff_audit;

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

            // T11：新会话（未续聊）run 边界重置会话级 findings 容器。serve
            // 长驻进程共享同一 Agent 时，容器此前只增不减（跨 run 累积），
            // 触顶（MAX_QUALITY_FINDINGS）后新 finding 永久丢弃；新会话起点
            // 清空容器，保证后续 run 的 Blocking finding 始终可见。并发 run
            // 的隔离由 run 局部暂存区（loop_impl 内 run_findings）承担，此处
            // 仅维护会话级容器的"不跨会话累积"语义。
            if !seeded {
                quality_findings.lock().await.clear();
            }

            // Inject the system prompt only on a fresh conversation. When the
            // store already holds prior turns, the system prompt is part of
            // them and re-injecting it would duplicate it. The default prompt
            // applies whenever the caller did not configure an override.
            if !seeded {
                // 用 CacheAwarePromptBuilder 构造稳定前缀（system prompt +
                // repo_map），消除与 context::PromptBuilder 的 repo map 拼接
                // 重复，并获得 prefix hash 用于 cache miss 诊断。tools 传空
                // （provider 层负责 schema 注入），project_memory 传 None
                // （agent 主路径不注入）。
                let repo_map_str: Option<String> = repo_map_provider
                    .as_ref()
                    .and_then(|p| p(&input.prompt))
                    .filter(|m| !m.is_empty());
                let mut cache_builder = deepseeknova_context::CacheAwarePromptBuilder::new(true);
                let (content, prefix_hash) =
                    cache_builder.build_prefix(&system_prompt, &[], None, repo_map_str.as_deref());
                tracing::info!(prefix_hash = %prefix_hash, "system prompt prefix constructed");
                memory.write().await.add_message(Message {
                    role: Role::System,
                    content,
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                    reasoning_signature: None,
                    usage: None,
                });
            }

            // Run-start 召回注入（仅新会话）：作为稳定 system 前缀之后的 volatile
            // User 消息插入 —— 保住 DeepSeek-V4 前缀缓存；无 tool_calls/tool_call_id/
            // reasoning，故通过 replay 不变量校验。
            if !seeded {
                if let Some(ref rp) = recall_provider {
                    // B4：新会话召回注入按预算裁剪。
                    inject_recall(
                        rp,
                        &mut *memory.write().await,
                        &input.prompt,
                        crate::tools::DEFAULT_RECALL_MAX_CHARS,
                    );
                }
            }

            // P3.3 中途检索：续聊轮次开头，上一轮有工具活动时注入一次
            // 记忆 + 代码图命中（query = 当前用户消息）。
            if seeded {
                if let Some(ref mid) = mid_run {
                    let active = !mid.require_tool_turn
                        || history_last_turn_used_tools(&memory.read().await.get_all());
                    if active {
                        // B4：续聊中途检索召回注入按预算裁剪。
                        inject_recall(
                            &mid.provider,
                            &mut *memory.write().await,
                            &input.prompt,
                            crate::tools::DEFAULT_RECALL_MAX_CHARS,
                        );
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
                turn_end_result_cap,
                preventive_shrink_ratio,
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
                cost_budget,
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
                diff_audit,
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
pub(crate) struct PendingToolCall {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) arguments: String,
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
/// 子代理步数预算（防烧 token）。
const ADVERSARIAL_REVIEW_MAX_STEPS: usize = 3;

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
mod loop_impl;
// 主循环对外仅需 4 个符号：run_agent_loop（Runner 装配 + 集成测试）、
// is_tool_error_result（测试）、fire_user_notify_hooks（Agent 构造/会话钩子）、
// MidRunRetrieval（Agent 字段类型）。其余回合级函数仅 loop_impl 内部使用。
pub(crate) use loop_impl::{
    fire_user_notify_hooks, is_tool_error_result, run_agent_loop, MidRunRetrieval,
};

// ---------------------------------------------------------------------------
// Tests — 主循环集成测试（Agent + MockProvider 端到端）+ Agent 结构单元测试。
// 纯函数类测试（classify / path / tools / approval / render / agent_diag）
// 已随 M7 拆分迁入各自子模块的 `#[cfg(test)]`。
// P0-A（2026-08-11）：主循环集成测试整体迁至同目录 tests.rs（纯行范围搬移，
// 无内容变更），本文件仅保留模块声明。
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
