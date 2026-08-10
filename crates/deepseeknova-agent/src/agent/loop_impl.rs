//! 主循环与回合处理（M7 拆分续）：`run_agent_loop` / `run_review_pass` /
//! `stream_and_process_turn` / 回合级工具执行与审查/反思/归因助手。
//!
//! 全部为自由函数：运行状态经参数传入，不触碰 [`super::Agent`] 私有字段，
//! 因此可从 `agent.rs` 整段迁出。`use super::*` 复用父模块的全部类型导入
//! 与辅助项（`maybe_spawn_adversarial_review` / `UserHooks` 等）。

use super::*;
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

/// 触发一组通知型用户 hooks（session_start / session_end / failure）：
/// 任一命令失败（非 0 退出 / 超时 / 崩溃）仅 warn，不阻断主流程。空列表
/// 零开销（不 spawn 进程）。
pub(crate) fn fire_user_notify_hooks(commands: &[UserHookCommand], payload: &HookPayload) {
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
                    quality_findings,
                    quality_start_len,
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
