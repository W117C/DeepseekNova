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
    async fn execute(
        &self,
        _ctx: &ToolContext,
        _args: &str,
    ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
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
        reasoning_signature: None,
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
// P2-4 团队级 USD 花费上限（CostBudget）
// -----------------------------------------------------------------------

/// 向一个 ledger 记入一条 big 模型的用量（USD 可由 2.0/8.0/0.2 单价估算）。
fn record_cost(
    ledger: &deepseeknova_provider::cost::CostLedger,
    prompt: u32,
    completion: u32,
    cache_hit: u32,
) {
    use deepseeknova_provider::cost::ModelRole;
    ledger.record(
        ModelRole::Main,
        "big",
        &deepseeknova_core::chunk::Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
            cache_hit_tokens: cache_hit,
            cache_miss_tokens: prompt.saturating_sub(cache_hit),
            reasoning_tokens: 0,
        },
    );
}

/// big 模型价格表（2.0 input / 8.0 output / 0.2 cache-hit，$/1M tokens）。
fn priced_table() -> deepseeknova_provider::cost::PriceTable {
    let mut prices = deepseeknova_provider::cost::PriceTable::new();
    prices.insert(
        "big".to_string(),
        deepseeknova_provider::cost::ModelPrices {
            input_per_mtok: Some(2.0),
            output_per_mtok: Some(8.0),
            cache_hit_per_mtok: Some(0.2),
        },
    );
    prices
}

/// USD 超限 → 第一步即 Paused，reason 带成本信息
/// （`budget: cost limit $X exceeded (spent $Y)`），对齐现有 `budget: <why>` 语义。
#[tokio::test]
async fn cost_budget_over_limit_emits_paused_with_cost_reason() {
    let ledger = Arc::new(deepseeknova_provider::cost::CostLedger::new());
    // 预置 0.5M prompt(0.25M hit) + 0.25M completion → 2.55 USD；上限 2.0 → 已超。
    record_cost(&ledger, 500_000, 250_000, 250_000);
    let agent = Agent::new(Arc::new(MockProvider::text("hi")), 5)
        .with_cost_budget(crate::budget::cost::CostBudget::new(
            Arc::clone(&ledger),
            priced_table(),
            2.0,
        ))
        .with_session_label("cost-sess-1");

    let events = drain(agent, "run").await;
    let paused: Vec<&String> = events
        .iter()
        .filter_map(|e| match e {
            RunEvent::Paused { reason, session_id } => {
                assert_eq!(session_id.as_deref(), Some("cost-sess-1"));
                Some(reason)
            }
            _ => None,
        })
        .collect();
    assert_eq!(paused.len(), 1, "超限应恰好 Paused 一次");
    assert!(
        paused[0].starts_with("budget: cost limit"),
        "reason 应对齐 budget: <why> 语义: {paused:?}"
    );
    assert!(paused[0].contains("2.0000"), "应含上限 $X: {}", paused[0]);
    assert!(paused[0].contains("2.5500"), "应含已花 $Y: {}", paused[0]);
    assert!(
        !events.iter().any(|e| matches!(e, RunEvent::Done(_))),
        "超限后不得再出 Done"
    );
}

/// 未超限 → 不 Paused，正常跑完（成本检查不影响既有流程）。
#[tokio::test]
async fn cost_budget_under_limit_continues_to_done() {
    let ledger = Arc::new(deepseeknova_provider::cost::CostLedger::new());
    // 预置 1000 prompt → 2.0/1M*1000 = 0.002 USD；上限 10 → 未超。
    record_cost(&ledger, 1_000, 0, 0);
    let provider = MockProvider::tool_call("spy", "{}", "ignored", "done");
    let mut agent = Agent::new(Arc::new(provider), 5).with_cost_budget(
        crate::budget::cost::CostBudget::new(Arc::clone(&ledger), priced_table(), 10.0),
    );
    agent.register_tool(Arc::new(SpyTool {
        name: "spy",
        result: "ran".into(),
    }));

    let events = drain(agent, "do the thing").await;
    assert!(
        events.iter().any(|e| matches!(e, RunEvent::Done(_))),
        "未超限应正常完成"
    );
    assert!(
        !events.iter().any(|e| matches!(e, RunEvent::Paused { .. })),
        "未超限不得 Paused"
    );
}

/// 与 token 预算共存（先到先停）：token 未触发、USD 已超 → 走成本 Paused。
#[tokio::test]
async fn cost_budget_wins_when_token_budget_still_allows() {
    let ledger = Arc::new(deepseeknova_provider::cost::CostLedger::new());
    record_cost(&ledger, 500_000, 250_000, 250_000); // 2.55 USD > 2.0 上限
    let agent = Agent::new(Arc::new(MockProvider::text("hi")), 5)
        // token 预算开着但远未触发。
        .with_budget(crate::budget::controller::PromptBudgetController {
            max_total_tokens: 1_000_000,
            max_memory_tokens: 32_000,
        })
        .with_cost_budget(crate::budget::cost::CostBudget::new(
            Arc::clone(&ledger),
            priced_table(),
            2.0,
        ));

    let events = drain(agent, "run").await;
    let reason = events
        .iter()
        .filter_map(|e| match e {
            RunEvent::Paused { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .next()
        .expect("应 Paused");
    assert!(
        reason.starts_with("budget: cost limit"),
        "先到先停：成本超限应先于 token 触发: {reason}"
    );
}

/// 与 token 预算共存（先到先停）：USD 未超、token 超限 → 走既有 token Paused。
#[tokio::test]
async fn token_budget_wins_when_cost_budget_still_allows() {
    let ledger = Arc::new(deepseeknova_provider::cost::CostLedger::new());
    record_cost(&ledger, 1_000, 0, 0); // 0.002 USD，远低于上限
                                       // 大 system prompt 把首步 current 顶进 Reject 窗口（< 0.8*max 且 +2048 > max）。
                                       // 默认 prompt ≈1104 token + 1.1w ASCII ≈3300 token → current ≈4400；max=6000
                                       // 时 0.8*max=4800 ≥ current、current+2048≈6454 > max → Reject。
    let agent = Agent::new(Arc::new(MockProvider::text("hi")), 5)
        .with_appended_system_prompt("a".repeat(11_000))
        .with_budget(crate::budget::controller::PromptBudgetController {
            max_total_tokens: 6_000,
            max_memory_tokens: 1_000,
        })
        .with_cost_budget(crate::budget::cost::CostBudget::new(
            Arc::clone(&ledger),
            priced_table(),
            10.0,
        ));

    let events = drain(agent, "run").await;
    let reason = events
        .iter()
        .filter_map(|e| match e {
            RunEvent::Paused { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .next()
        .expect("token 超限应 Paused");
    assert!(
        reason.starts_with("budget:") && !reason.contains("cost limit"),
        "先到先停：token 超限应先触发（保留既有 reason 语义）: {reason}"
    );
}

/// 无单价（成本不可估算）→ 上限退化为不生效，正常完成（fail-open）。
#[tokio::test]
async fn cost_budget_no_price_data_is_noop() {
    let ledger = Arc::new(deepseeknova_provider::cost::CostLedger::new());
    record_cost(&ledger, 1_000_000, 0, 0);
    let agent = Agent::new(Arc::new(MockProvider::text("hi")), 5).with_cost_budget(
        crate::budget::cost::CostBudget::new(
            Arc::clone(&ledger),
            deepseeknova_provider::cost::PriceTable::new(),
            0.0,
        ),
    );

    let events = drain(agent, "run").await;
    assert!(
        events.iter().any(|e| matches!(e, RunEvent::Done(_))),
        "无单价时成本上限不得阻断"
    );
    assert!(!events.iter().any(|e| matches!(e, RunEvent::Paused { .. })));
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
    let agent = Agent::new(Arc::new(MockProvider::text("all good")), 3).with_diagnose_hook(hook);

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
        async fn execute(
            &self,
            _ctx: &ToolContext,
            _args: &str,
        ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
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
fn default_system_prompt_defines_provider_neutral_execution_contract() {
    assert!(
        DEFAULT_SYSTEM_PROMPT.contains("Read before writing"),
        "default prompt must require reading before edits"
    );
    assert!(
        DEFAULT_SYSTEM_PROMPT.contains("permission"),
        "default prompt must acknowledge permission boundaries"
    );
    assert!(
        DEFAULT_SYSTEM_PROMPT.contains("Keep unrelated user changes intact"),
        "default prompt must preserve unrelated changes"
    );
    assert!(
        !DEFAULT_SYSTEM_PROMPT.contains("DeepSeek-V4"),
        "default prompt must not be tied to a provider model"
    );
    assert!(
        !DEFAULT_SYSTEM_PROMPT.contains("one action per turn"),
        "default prompt must not prohibit valid parallel tool use"
    );
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
    let agent =
        Agent::new(Arc::new(MockProvider::text("ok")), 3).with_appended_system_prompt("EXTRA_HINT");
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
            if result.contains("LSP diagnostics for src/a.rs") && result.contains("error: boom") {
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
    let agent =
        Arc::new(Agent::new(Arc::new(MockProvider::text("fallback")), 3).with_auto_router(decider));

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
    async fn execute(
        &self,
        _ctx: &ToolContext,
        _args: &str,
    ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
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

/// 审批 responder 桩：记录收到的描述（应含风险前缀）并拒绝。
struct CapturingResponder {
    seen: Arc<std::sync::Mutex<Option<String>>>,
}

#[async_trait::async_trait]
impl deepseeknova_core::runner::ApprovalResponder for CapturingResponder {
    async fn request(&self, _id: &str, _title: &str, description: Option<&str>) -> bool {
        *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = description.map(ToOwned::to_owned);
        false
    }
}

/// 两轮脚本：调用 bash（写命令），随后文本收尾。
fn call_bash_then_done() -> Vec<Vec<Chunk>> {
    vec![
        vec![
            Chunk::ToolCallStart {
                id: "c1".into(),
                name: "bash".into(),
            },
            Chunk::ToolCallEnd {
                id: "c1".into(),
                name: "bash".into(),
                arguments: r#"{"command":"rm -rf /tmp/x"}"#.into(),
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

/// 风险标签端到端断言：Ask 裁决 → ApprovalRequest 描述 → responder
/// 收到的参数必须携带 `[风险:非只读]` 前缀（BLOCKED 点名的唯一测试缺口）。
#[tokio::test]
async fn ask_risk_prefix_reaches_approval_responder() {
    let seen = Arc::new(std::sync::Mutex::new(None::<String>));
    let provider = Arc::new(MockProvider::sequential(call_bash_then_done()));
    let mut agent = Agent::new(provider, 5)
        .with_permission_gate(ask_mode_gate())
        .with_approval_responder(Arc::new(CapturingResponder { seen: seen.clone() }));
    agent.register_tool(Arc::new(BashSpy { fail: false }));

    let mut stream = agent
        .run_stream(RunInput {
            prompt: "go".into(),
            images: vec![],
            model_override: None,
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let desc = seen
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .expect("Ask 决策必须调用审批 responder");
    assert!(
        desc.contains("[风险:非只读]"),
        "responder 描述必须携带风险前缀, got: {desc}"
    );
    assert!(
        desc.contains(r#"{"command":"rm -rf /tmp/x"}"#),
        "responder 描述必须保留原始调用参数, got: {desc}"
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
        async fn execute(
            &self,
            ctx: &ToolContext,
            _args: &str,
        ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
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
        ) -> Result<Message, deepseeknova_core::DeepseeknovaError> {
            Ok(Message {
                role: Role::Assistant,
                content: "done".into(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                reasoning_signature: None,
            })
        }
        async fn stream(
            &self,
            v: deepseeknova_provider::ValidatedRequest<'_>,
        ) -> Result<deepseeknova_core::chunk::ChunkStream, deepseeknova_core::DeepseeknovaError>
        {
            *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = v.messages.to_vec();
            let chunks: Vec<
                Result<deepseeknova_core::chunk::Chunk, deepseeknova_core::DeepseeknovaError>,
            > = vec![
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
            responses: std::sync::Mutex::new(responses.into_iter().map(str::to_string).collect()),
        }
    }
}

#[async_trait::async_trait]
impl deepseeknova_provider::Provider for SeqProvider {
    async fn generate(
        &self,
        _v: deepseeknova_provider::ValidatedRequest<'_>,
    ) -> Result<Message, deepseeknova_core::DeepseeknovaError> {
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
            reasoning_signature: None,
        })
    }
    async fn stream(
        &self,
        _v: deepseeknova_provider::ValidatedRequest<'_>,
    ) -> Result<deepseeknova_core::chunk::ChunkStream, deepseeknova_core::DeepseeknovaError> {
        return Err(deepseeknova_core::DeepseeknovaError::provider(
            "SeqProvider is generate-only (review path)",
        ));
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
    fn after(&self, _ctx: &ToolHookCtx, _call: &ToolCall, _result: &str) -> Vec<QualityFinding> {
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
    async fn execute(
        &self,
        _ctx: &ToolContext,
        _args: &str,
    ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
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
    async fn execute(
        &self,
        _ctx: &ToolContext,
        _args: &str,
    ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
        if self.fail {
            return Err(deepseeknova_core::DeepseeknovaError::tool(
                "command exited with code 1".to_string(),
            ));
        }
        Ok("ok".to_string())
    }
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
    async fn execute(
        &self,
        _ctx: &ToolContext,
        _args: &str,
    ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
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
    async fn execute(
        &self,
        _ctx: &ToolContext,
        _args: &str,
    ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
        return Err(deepseeknova_core::DeepseeknovaError::tool(
            "boom".to_string(),
        ));
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
    let mut agent = Agent::new(high.clone(), 5).with_effort_routing(quick.clone(), high.clone());
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
    let mut agent = Agent::new(high.clone(), 5).with_effort_routing(quick.clone(), high.clone());
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
            reasoning_signature: None,
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
            reasoning_signature: None,
        },
        Message {
            role: Role::Tool,
            content: "ok".into(),
            name: None,
            tool_calls: None,
            tool_call_id: Some("x1".into()),
            reasoning_content: None,
            reasoning_signature: None,
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
    fn after(&self, _ctx: &ToolHookCtx, _call: &ToolCall, _result: &str) -> Vec<QualityFinding> {
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
        false,      // diff_audit（A4）：测试默认关
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
    let agent =
        Agent::new(Arc::new(MockProvider::text("done")), 3).with_user_hooks(UserHooks::default());
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
