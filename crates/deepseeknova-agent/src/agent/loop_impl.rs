//! 主循环与回合处理（M7 拆分续）：`run_agent_loop` / `run_review_pass` /
//! `stream_and_process_turn` / 回合级工具执行与审查/反思/归因助手。
//!
//! 全部为自由函数：运行状态经参数传入，不触碰 [`super::Agent`] 私有字段，
//! 因此可从 `agent.rs` 整段迁出。`use super::*` 复用父模块的全部类型导入
//! 与辅助项（`maybe_spawn_adversarial_review` / `UserHooks` 等）。

use super::*;
use deepseeknova_core::tool_hook::run_user_hook_sync;
use deepseeknova_core::DeepseeknovaError;

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
    // 历史 run 的 findings 不得触发或进入本 run 的对抗审查证据。T11 起
    // 调用方传入**本 run 局部 findings 暂存区**（start_len=0），并发 run
    // 各归各 run，不再依赖「会话容器长度差分」近似。
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

/// A4：写后 diff 规模审计的变更行阈值。超过则告警注入 ToolResult
/// （确定性提示"最小改动"，替代提示词层职责）。git 仓库内生效。
const DIFF_AUDIT_MAX_CHANGED_LINES: usize = 300;

/// A4：统计 diff 文本的变更行数（`+`/`-` 打头的行，排除 `+++`/`---`
/// 文件头）。纯函数，便于单测。
fn changed_line_count(diff: &str) -> usize {
    diff.lines()
        .filter(|l| {
            let t = l.trim_start();
            (t.starts_with('+') && !t.starts_with("+++"))
                || (t.starts_with('-') && !t.starts_with("---"))
        })
        .count()
}

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

/// 触发一组通知型用户 hooks（session_start / session_end / failure /
/// tool_after）：任一命令失败（非 0 退出 / 超时 / 崩溃）仅 warn，不阻断
/// 主流程。空列表零开销（不 spawn 进程）。
///
/// 保持同步签名：session / failure 事件可能经 MetricsGuard 的 Drop 路径
/// 触发（Drop 中无法 await）。为避免阻塞 tokio worker（T19），多线程
/// runtime 上用 `block_in_place` 把当前线程让出 worker 池再同步执行；
/// current_thread / 无 runtime 上下文退化为直接同步执行（本就没有可让出
/// 的 worker）。阻塞完成语义不变：返回时钩子已执行完毕。
pub(crate) fn fire_user_notify_hooks(commands: &[UserHookCommand], payload: &HookPayload) {
    if commands.is_empty() {
        return;
    }
    let run_blocking = || {
        for cmd in commands {
            let run = run_user_hook_sync(cmd, payload);
            if !run.exec.is_allowed() {
                warn!(
                    "user hook '{}' ({}) failed: {:?}",
                    cmd.command, payload.event, run.exec
                );
            }
        }
    };
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            // T19：多线程 runtime 上把当前线程让出 worker 池再阻塞执行，
            // 通知型钩子不占用 worker，其他任务可继续推进。
            tokio::task::block_in_place(run_blocking);
        }
        _ => {
            // current_thread runtime / 无 runtime 上下文：直接同步执行。
            run_blocking();
        }
    }
}

pub(crate) async fn run_agent_loop(
    provider: Arc<dyn Provider>,
    tools: Vec<Arc<dyn Tool>>,
    max_steps: usize,
    compaction_threshold: Option<u32>,
    memory: Arc<tokio::sync::RwLock<Memory>>,
    input: RunInput,
    tx: &mpsc::Sender<Result<RunEvent, DeepseeknovaError>>,
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
    cost_budget: Option<crate::budget::cost::CostBudget>,
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
    diff_audit: bool,
) -> Result<(), DeepseeknovaError> {
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
        reasoning_signature: None,
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
    // T11：run 局部 findings 暂存区——工具钩子写会话容器时同步写入本暂存区；
    // 所有 run 级消费方（MetricsGuard/DiagnoseGuard/对抗审查/协议门）只读本
    // 暂存区，起点恒为 0。并发 run 共享同一会话容器时各归各 run，不再用
    // 「容器长度差分」近似（F4 的 try_lock 缓解只是降级，交错切片仍可能把
    // 另一 run 的 findings 误切进本 run）；且每次 run 独立起步，会话容器
    // 即使触顶（F9 上限）也不会让新 run 的 Blocking finding 被丢弃。
    let run_findings: Arc<tokio::sync::Mutex<Vec<QualityFinding>>> =
        Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let mut metrics = MetricsGuard::new(
        metrics_hook,
        &run_findings,
        session_label.clone(),
        &user_hooks.failure,
        workspace_root.clone(),
    );
    // 任务质量闭环 B：失败诊断采集（Paused/failed 结束路径产出报告；
    // 成功路径 suppress 关闭；Drop 兜底异常路径）。findings 同样接本 run
    // 局部暂存区（start_len=0 = 只含本 run），并发/历史 run 互不污染。
    let mut diagnose = DiagnoseGuard::new(
        diagnose_hook,
        session_label.clone(),
        Arc::clone(&run_findings),
        0,
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

    // P0.6：工具索引（name → Arc<Tool>）在 run 内不变（tools 是函数参数，
    // 循环内不增删）——提前构建一次，消除每步重建的 schema().name 开销。
    let tool_map: HashMap<String, Arc<dyn Tool>> = tools
        .iter()
        .map(|t| (t.schema().name.clone(), Arc::clone(t)))
        .collect();

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
            return Err(DeepseeknovaError::runner(format!(
                "exceeded max execution time ({:?})",
                security.limits.max_execution_time
            )));
        }
        if tool_calls_made >= security.limits.max_tool_calls {
            warn!(
                "agent exceeded max_tool_calls ({})",
                security.limits.max_tool_calls
            );
            return Err(DeepseeknovaError::runner(format!(
                "exceeded max tool calls ({})",
                security.limits.max_tool_calls
            )));
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
            // 计算记忆注入侧 token（<recalled-memory> 标签的 User 消息），
            // 用于 max_memory_tokens 独立预算判定。
            let memory_tokens: usize = snapshot
                .iter()
                .filter(|m| {
                    m.role == deepseeknova_core::Role::User
                        && m.content.contains("<recalled-memory>")
                })
                .map(|m| crate::tokens::estimate_text_tokens(&m.content) as usize)
                .sum();
            use crate::budget::controller::BudgetDecision;
            match b.evaluate_budget(current, EXPECTED_TURN_TOKENS, memory_tokens) {
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
                        &run_findings,
                        0,
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

        // 团队级 USD 花费上限（P2-4）：与 token 预算并列，step 边界同样评估，
        // 任一触发即停（先到先停）。成本无法估算（模型无单价）时退化为不生效。
        if let Some(ref cb) = cost_budget {
            if let Some((limit, spent)) = cb.exceeded() {
                warn!("cost budget exceeded: spent ${spent:.4} >= limit ${limit:.4}");
                metrics.emit(None);
                diagnose.record_failure(
                    "budget",
                    None,
                    None,
                    format!("cost limit exceeded: spent ${spent:.4} >= limit ${limit:.4}"),
                );
                // Bugbot #2：Paused 终端分支同样接对抗审查（spec §4.2
                // 无 unverified 限定）；须在 emit 之前注入。
                wire_session_adversarial_review(
                    &mut diagnose,
                    provider.clone(),
                    adversarial_review_enabled,
                    &input.prompt,
                    &*memory.read().await,
                    &run_findings,
                    0,
                )
                .await;
                diagnose
                    .emit("paused", &memory.read().await.get_all())
                    .await;
                tx.send(Ok(RunEvent::Paused {
                    reason: format!("budget: cost limit ${limit:.4} exceeded (spent ${spent:.4})"),
                    session_id: session_label.clone(),
                }))
                .await
                .ok();
                return Ok(());
            }
        }

        // Atomic Turn-end compaction
        // B2（写前阻塞）：压缩在 turn 末同步执行（L1/L2/L3 全链路 await 完成
        // 后才进入下一轮），而写工具执行在 turn 内 `stream_and_process_turn`
        // ——压缩与工具执行天然串行，压缩期间不存在并发写窗口（对齐 Codex
        // "压缩前阻塞写工具"的语义由本架构顺序保证，无需额外锁）。
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
                // F4：压缩事件观测（L1 截断）。
                diagnose.record_compaction("L1", threshold.max(1), 0);

                info!("shrunk tool results: {} -> {} tokens", before, after_shrink);

                if after_shrink > threshold {
                    warn!("context still over threshold after shrinking tool results. sliding window...");
                    memory.write().await.slide_window();
                    compacted = true;
                    let after_slide = memory.read().await.estimate_tokens();
                    info!("slid window: {} -> {} tokens", after_shrink, after_slide);
                    // F4：压缩事件观测（L2 滑动窗口）。
                    diagnose.record_compaction("L2", threshold, 0);

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
                            // F4：压缩事件观测（L3 LLM 摘要；digest 长度取压缩后
                            // 首条消息字符数）。
                            let digest_chars = memory
                                .read()
                                .await
                                .iter_all()
                                .next()
                                .map(|m| m.content.len())
                                .unwrap_or(0);
                            diagnose.record_compaction("L3", threshold, digest_chars);
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
                            // B4：压缩后召回注入按预算裁剪（防记忆块反噬上下文）。
                            inject_recall(
                                rp,
                                &mut *memory.write().await,
                                &q.content,
                                crate::tools::DEFAULT_RECALL_MAX_CHARS,
                            );
                            // 召回注入修改了历史 → 再次快照。
                            snapshot = memory.read().await.get_all();
                        }
                    }
                }
            }
        }

        // P0.6：tool_map 已在循环前构建一次（run 内工具集不变），此处复用。

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
                // Bugbot #12：findings 接 run 局部质量快照（首轮通常为空，
                // 但自定义门可依赖 ctx.findings 的非空语义）。T11：只读
                // 本 run 暂存区，并发 run 各归各 run。
                let findings = run_findings.lock().await.clone();
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
            let findings = run_findings.lock().await.clone();
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
            &run_findings,
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
            diff_audit,
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
                    if wrote_files {
                        // A8：验证命令发现——配置 `[verify] commands` 为空时，
                        // 从会话工具结果推断验证命令（cargo check/test 等）；
                        // 显式配置仍优先（commands 非空直接用配置）。
                        let effective: std::borrow::Cow<crate::verify::VerifySettings> =
                            if vs.commands.is_empty() {
                                let tool_results: Vec<String> = memory
                                    .read()
                                    .await
                                    .iter_all()
                                    .filter(|m| m.role == Role::Tool)
                                    .map(|m| m.content.clone())
                                    .collect();
                                let inferred = crate::verify::infer_verify_commands(&tool_results);
                                if inferred.is_empty() {
                                    std::borrow::Cow::Borrowed(vs)
                                } else {
                                    let mut s = vs.clone();
                                    s.commands = inferred;
                                    std::borrow::Cow::Owned(s)
                                }
                            } else {
                                std::borrow::Cow::Borrowed(vs)
                            };
                        if !effective.commands.is_empty() {
                            // 任务质量闭环 B：verify 相位起点（报告阶段时间戳）。
                            diagnose.phase_enter("verify");
                            match crate::verify::run_verify_pass(
                                &tool_map,
                                &effective,
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
                                        // Bugbot #12：findings 接 run 局部质量快照。
                                        let findings = run_findings.lock().await.clone();
                                        let ctx =
                                            phase_runner.build_ctx(Phase::Verify, true, findings);
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
                                            tx.send(Ok(RunEvent::GateViolation(v.clone())))
                                                .await
                                                .ok();
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
                                        reasoning_signature: None,
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
                                        &run_findings,
                                        0,
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
                                        reasoning_signature: None,
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
                                        &run_findings,
                                        0,
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
                        // A7：grounding 证据 = 会话工具结果文本。先绑定局部
                        // 变量，使 memory 读锁 guard 在进入 await 前释放——
                        // 否则临时 guard 存活到 match 结束，Issues 分支的
                        // memory.write() 会与读锁死锁（tokio RwLock 不可重入）。
                        let grounding_evidence = evidence_for_grounding(&*memory.read().await);
                        match run_review_pass(
                            rp.as_ref(),
                            rs,
                            &workspace_root,
                            &input.prompt,
                            &output.text,
                            &grounding_evidence,
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
                                    reasoning_signature: None,
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
                    // Bugbot #12：findings 接 run 局部质量快照。
                    let findings = run_findings.lock().await.clone();
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
                    &run_findings,
                    0,
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
                        &run_findings,
                        0,
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
                return Err(DeepseeknovaError::runner(format!(
                    "reached max steps ({max_steps}) without completing the task"
                )));
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
            &run_findings,
            0,
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
    Err(DeepseeknovaError::runner(format!(
        "reached max steps ({max_steps}) without completing the task"
    )))
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

/// A7：收集会话工具结果文本作为 grounding 审查证据（Role::Tool 消息的
/// content）。审查时若完成声明含无工具证据的成功断言（如 "all tests pass"
/// 但无对应测试输出），降级为 Issues。
fn evidence_for_grounding(memory: &crate::memory::Memory) -> Vec<String> {
    memory
        .iter_all()
        .filter(|m| m.role == Role::Tool)
        .map(|m| m.content.clone())
        .collect()
}

/// 执行一次审查：采集 diff → 问审查模型 → 判定。任何失败 → Skipped。
/// `first_pass` 仅用于 review_triggered 只计首轮。
/// A7：`evidence` 为本会话工具结果文本（memory 中 Role::Tool 消息），用于
/// grounding 审查——审查模型判定 Approve 时，若完成声明含无证据的成功断言
/// （如 "all tests pass" 但无对应工具结果），降级为 Issues（确定性审查，
/// 替代提示词层"不要编造事实"职责）。
async fn run_review_pass(
    provider: &dyn Provider,
    settings: &crate::review::ReviewSettings,
    workspace_root: &std::path::Path,
    task: &str,
    completion: &str,
    evidence: &[String],
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
        Some(crate::review::Verdict::Approve) => {
            // A7：grounding 审查——完成声明含无证据的成功断言时**告警但不降级**
            // （启发式标记，非阻断门；降级会改变 review 判定语义，破坏
            // "issues 后修复 → Done" 的既有闭环）。命中以 warn + counter 记录，
            // 供诊断/观测；后续可演进为配置化阻断。
            let ungrounded = crate::review::find_ungrounded_assertions(completion, evidence);
            if !ungrounded.is_empty() {
                bump("grounding_issues");
                let detail: Vec<String> = ungrounded
                    .iter()
                    .map(|p| crate::review::render_ungrounded_issue(p))
                    .collect();
                tracing::warn!(
                    "grounding review: {} ungrounded assertion(s) in completion — \
                     flagged but not blocking",
                    detail.len()
                );
            }
            ReviewOutcome::Approve
        }
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
    tx: &mpsc::Sender<Result<RunEvent, DeepseeknovaError>>,
    cancel: &CancellationToken,
    workspace_root: &std::path::Path,
    security: &SecurityContext,
    tool_calls_made: &mut usize,
    wrote_files: &mut bool,
    tool_hooks: &[Arc<dyn ToolHook>],
    user_hooks: &UserHooks,
    session_id: &str,
    quality_findings: &Arc<tokio::sync::Mutex<Vec<QualityFinding>>>,
    run_findings: &Arc<tokio::sync::Mutex<Vec<QualityFinding>>>,
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
    diff_audit: bool,
) -> Result<StepOutcome, DeepseeknovaError> {
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
            DeepseeknovaError::runner(format!(
                "history replay invariant violated: {} violation(s) detected",
                violations.len()
            ))
        },
    )?;

    let mut stream = provider.stream(validated).await?;

    let mut text_buf = String::new();
    let mut reasoning_buf = String::new();
    // T12 收尾：流式 signature（signature_delta）随 reasoning 一并保存，
    // 组装 assistant 消息时写入 reasoning_signature（多轮回放必需）。
    let mut reasoning_signature: Option<String> = None;
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
                if reasoning_signature.is_none() {
                    reasoning_signature = signature.clone();
                }
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
            reasoning_signature: reasoning_signature.clone(),
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
            reasoning_signature: reasoning_signature.clone(),
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
            // Bugbot #12：findings 接 run 局部质量快照。
            let findings = run_findings.lock().await.clone();
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
                                    // 风险标签同时进 RunEvent 描述（serve/HTTP）
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
                    let run = run_user_hook(cmd, &payload).await;
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
                // A4：写后 diff 规模审计（git 仓库内，可配置默认关）。变更行数
                // 超阈值 → 告警注入 ToolResult（确定性提示"最小改动"，替代提示词
                // 层 Make Focused Progress 职责）。非 git 仓库 collect_diff 返回
                // None，静默跳过。ToolHook::after 为同步方法无法 await git，故
                // 审计落在主循环回填段（与 lsp 诊断同点位）。
                if diff_audit {
                    if let Some(diff) = crate::review::collect_diff(workspace_root, 64 * 1024).await
                    {
                        let changed = changed_line_count(&diff);
                        if changed > DIFF_AUDIT_MAX_CHANGED_LINES {
                            result.push_str(&format!(
                                "\n\n---\n[diff audit] warning: {changed} changed lines exceeds \
                                 the {DIFF_AUDIT_MAX_CHANGED_LINES}-line budget — consider smaller, \
                                 verifiable changes"
                            ));
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
                    // 事件流照常发出（用户可见），仅不入累计。
                    // T11：会话容器与 run 局部暂存区**同步**写入（同一批次内
                    // 按同一上限截断）。run 局部暂存区供本 run 的
                    // MetricsGuard/DiagnoseGuard/对抗审查/协议门消费，起点恒
                    // 为 0——并发 run 各归各 run，不再依赖「会话容器长度差分」
                    // 近似；且每次 run 独立起步，即使会话容器已触顶，新 run 的
                    // Blocking finding 依然进入本 run 暂存区（可见、可诊断）。
                    let mut dropped_warned = false;
                    let mut emitted = Vec::with_capacity(findings.len());
                    {
                        let mut qf = quality_findings.lock().await;
                        let mut rlf = run_findings.lock().await;
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
                            if rlf.len() >= MAX_QUALITY_FINDINGS {
                                if !dropped_warned {
                                    warn!(
                                        "quality findings exceeded cap ({}); dropping new findings",
                                        MAX_QUALITY_FINDINGS
                                    );
                                    dropped_warned = true;
                                }
                            } else {
                                rlf.push(finding.clone());
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
                reasoning_signature: None,
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
        // T12 接线补面：工具执行包 select!——父取消（cancel 令牌）立即打断
        // 阻塞中的工具（future drop），返回取消错误文本。子代理经
        // `run_stream_with_parent_cancel` 传入的父令牌在此生效；顶层 run 的
        // Ctrl-C 取消同路径处理（此前主循环工具执行无取消分支，阻塞工具
        // 无法被父取消中断）。
        match tokio::select! {
            r = tool.execute(&ctx, &call.arguments) => r,
            _ = cancel.cancelled() => Err(deepseeknova_core::DeepseeknovaError::Cancelled),
        } {
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

/// P3.3 中途检索设置。
#[derive(Clone)]
pub(crate) struct MidRunRetrieval {
    pub(crate) provider: RecallProvider,
    pub(crate) require_tool_turn: bool,
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
        reasoning_signature: None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::MockProvider;
    use deepseeknova_core::types::ToolSchema;

    /// A4：changed_line_count 统计 +/- 变更行、排除 +++/--- 文件头。
    #[test]
    fn changed_line_count_counts_additions_and_removals() {
        let diff = "--- a/x.rs\n+++ b/x.rs\n@@ -1 +1 @@\n-old line\n+new line\n context\n";
        assert_eq!(
            changed_line_count(diff),
            2,
            "2 changed lines, 1 file header pair"
        );
        assert_eq!(changed_line_count(""), 0);
        assert_eq!(changed_line_count("@@ -1 +1 @@\n context\n"), 0);
        // 文件头 +++/--- 不得计入。
        assert_eq!(changed_line_count("--- a\n+++ b\n"), 0);
    }
    use std::sync::Arc;

    /// 固定返回给定文本的写类工具桩（驱动工具钩子产 findings）。
    struct OkTool {
        name: &'static str,
        result: String,
    }

    #[async_trait::async_trait]
    impl Tool for OkTool {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: self.name.to_string(),
                description: "ok tool".to_string(),
                parameters: serde_json::json!({"type":"object","properties":{}}),
            }
        }
        async fn execute(
            &self,
            _ctx: &ToolContext,
            _args: &str,
        ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
            Ok(self.result.clone())
        }
    }

    /// 执行前在两个并发 run 之间同步的写类工具桩：双方都进入才放行，强制
    /// 两 run 的工具执行 / 写 findings 交错（T11 并发交叉测试用）。
    struct BarrierOkTool {
        barrier: Arc<tokio::sync::Barrier>,
        result: String,
    }

    #[async_trait::async_trait]
    impl Tool for BarrierOkTool {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: "write_file".to_string(),
                description: "barrier tool".to_string(),
                parameters: serde_json::json!({"type":"object","properties":{}}),
            }
        }
        async fn execute(
            &self,
            _ctx: &ToolContext,
            _args: &str,
        ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
            self.barrier.wait().await;
            Ok(self.result.clone())
        }
    }

    /// after 恒产出 1 条 Blocking finding 的钩子。
    struct OneFindingHook;

    impl ToolHook for OneFindingHook {
        fn name(&self) -> &str {
            "one-finding-hook"
        }
        fn after(
            &self,
            _ctx: &ToolHookCtx,
            _call: &ToolCall,
            _result: &str,
        ) -> Vec<QualityFinding> {
            vec![QualityFinding {
                rule: "run-hook".into(),
                severity: FindingSeverity::Blocking,
                passed: false,
                evidence: "run".into(),
            }]
        }
    }

    /// after 恒产出 N 条 Warning finding 的钩子（超限截断测试）。
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

    /// 直连 `run_agent_loop` 的最小装配（单工具轮 + 完成文本）。事件流由
    /// 独立 drain 任务并发消费，避免 64 缓冲填满阻塞 findings 写回。
    async fn run_loop_once(
        provider: Arc<dyn Provider>,
        tools: Vec<Arc<dyn Tool>>,
        tool_hooks: Vec<Arc<dyn ToolHook>>,
        quality_findings: &Arc<tokio::sync::Mutex<Vec<QualityFinding>>>,
        metrics_hook: Option<MetricsHook>,
    ) {
        let (tx, mut rx) = mpsc::channel(64);
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let memory = Arc::new(tokio::sync::RwLock::new(Memory::new()));
        let cancel = CancellationToken::new();
        let _ = run_agent_loop(
            provider,
            tools,
            5,
            None,
            memory,
            RunInput {
                prompt: "write a file".into(),
                images: vec![],
                model_override: None,
            },
            &tx,
            &cancel,
            std::env::temp_dir(),
            SecurityContext::with_safe_defaults(),
            None,
            None,
            true,
            Vec::new(),
            true,
            true,
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
            None,
            None,
            None,
            None,
            false,
            None,
            metrics_hook,
            None,
            &tool_hooks,
            &UserHooks::default(),
            quality_findings,
            None,
            Vec::new(),
            false,
            false, // diff_audit：测试默认关
        )
        .await;
    }

    /// T11：并发交叉——两 run 共享同一会话容器并发执行，scorecard 各归各 run
    /// （不把另一 run 的 findings 误切进本 run）。修复前 MetricsGuard 依赖
    /// 「会话容器长度差分」，交错切片可能把并发 run 的 findings 混进本 run；
    /// 修复后各 run 消费自己的 run 局部暂存区，恒只含本 run。
    #[tokio::test]
    async fn concurrent_runs_scorecards_are_run_isolated() {
        let quality_findings = Arc::new(tokio::sync::Mutex::new(Vec::<QualityFinding>::new()));
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let fired = Arc::new(std::sync::Mutex::new(Vec::<QualitySummary>::new()));

        let run_one = |summaries: Arc<std::sync::Mutex<Vec<QualitySummary>>>,
                       barrier: Arc<tokio::sync::Barrier>| {
            let provider = Arc::new(MockProvider::tool_call(
                "write_file",
                r#"{"path":"src/a.rs","content":"x"}"#,
                "written",
                "done",
            ));
            let tool: Arc<dyn Tool> = Arc::new(BarrierOkTool {
                barrier,
                result: "written".into(),
            });
            let hook: MetricsHook = {
                let summaries = summaries.clone();
                Arc::new(move |_snap: SessionSnapshot, summary: QualitySummary| {
                    summaries
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(summary);
                })
            };
            let tool_hooks: Vec<Arc<dyn ToolHook>> = vec![Arc::new(OneFindingHook)];
            run_loop_once(
                provider,
                vec![tool],
                tool_hooks,
                &quality_findings,
                Some(hook),
            )
        };

        let a = run_one(fired.clone(), barrier.clone());
        let b = run_one(fired.clone(), barrier.clone());
        tokio::join!(a, b);

        let summaries = fired.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            summaries.len(),
            2,
            "two concurrent runs must fire two summaries"
        );
        let total: usize = summaries.iter().map(|s| s.findings.len()).sum();
        assert_eq!(total, 2, "total findings across both summaries must be 2");
        for s in summaries.iter() {
            assert_eq!(
                s.findings.len(),
                1,
                "each run's summary must contain exactly its own finding"
            );
        }
    }

    /// T11：触顶后新 run 的 Blocking finding 仍可见——run 1 灌满会话容器
    /// （MAX_QUALITY_FINDINGS 上限），run 2 的 Blocking finding 仍进入本 run
    /// 局部暂存区并被 scorecard 看到（修复前被 F9 超限丢弃，serve 长驻进程
    /// 的后续 run 的 scorecard/diagnose 全丢）。
    #[tokio::test]
    async fn new_run_blocking_finding_visible_after_session_cap() {
        let quality_findings = Arc::new(tokio::sync::Mutex::new(Vec::<QualityFinding>::new()));
        let fired = Arc::new(std::sync::Mutex::new(Vec::<QualitySummary>::new()));

        // run 1：灌满会话容器（超限截断，事件流全发）。
        {
            let provider = Arc::new(MockProvider::tool_call(
                "write_file",
                r#"{"path":"src/a.rs","content":"x"}"#,
                "written",
                "done",
            ));
            let tool: Arc<dyn Tool> = Arc::new(OkTool {
                name: "write_file",
                result: "written".into(),
            });
            let hook: MetricsHook = {
                let fired = fired.clone();
                Arc::new(move |_snap: SessionSnapshot, summary: QualitySummary| {
                    fired
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(summary);
                })
            };
            let tool_hooks: Vec<Arc<dyn ToolHook>> = vec![Arc::new(FloodFindingHook {
                count: MAX_QUALITY_FINDINGS + 1,
            })];
            run_loop_once(
                provider,
                vec![tool],
                tool_hooks,
                &quality_findings,
                Some(hook),
            )
            .await;
        }

        // run 2：单条 Blocking finding —— 会话容器虽已触顶，本 run 暂存区
        // 独立起步，scorecard 仍可见。
        {
            let provider = Arc::new(MockProvider::tool_call(
                "write_file",
                r#"{"path":"src/b.rs","content":"y"}"#,
                "written",
                "done",
            ));
            let tool: Arc<dyn Tool> = Arc::new(OkTool {
                name: "write_file",
                result: "written".into(),
            });
            let hook: MetricsHook = {
                let fired = fired.clone();
                Arc::new(move |_snap: SessionSnapshot, summary: QualitySummary| {
                    fired
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(summary);
                })
            };
            let tool_hooks: Vec<Arc<dyn ToolHook>> = vec![Arc::new(OneFindingHook)];
            run_loop_once(
                provider,
                vec![tool],
                tool_hooks,
                &quality_findings,
                Some(hook),
            )
            .await;
        }

        let summaries = fired.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(summaries.len(), 2, "two runs must fire two summaries");
        assert_eq!(
            summaries[0].findings.len(),
            MAX_QUALITY_FINDINGS,
            "run 1 (flood) summary capped at MAX_QUALITY_FINDINGS"
        );
        assert_eq!(
            summaries[1].findings.len(),
            1,
            "run 2 must still see its own Blocking finding after session cap"
        );
        assert!(
            summaries[1].findings[0].severity == FindingSeverity::Blocking,
            "run 2 finding must be Blocking"
        );
    }
}
