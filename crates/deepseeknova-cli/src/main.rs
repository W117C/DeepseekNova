mod chat;
mod cli;
mod eval;
mod init;
mod mcp_probe;
mod setup;
mod tui_undo;

use async_trait::async_trait;
use clap::Parser;
use cli::{Cli, Commands};
use deepseeknova_agent::{CoordinatorRunner, PlanModeRunner};
use deepseeknova_core::planner::SimplePlanner;
use deepseeknova_core::runner::{RunEventStream, RunInput, Runner};
use deepseeknova_core::RunEvent;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_stream::StreamExt;
use tracing::info;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::FmtSubscriber;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // TUI 全屏模式（alternate screen）下 stdout 被 ratatui 独占，任何
    // tracing 输出都会直接打进画面破坏布局，必须在安装 subscriber 前判定。
    let tui_mode = matches!(&cli.command, Some(Commands::Chat { tui: true, .. }));

    // Config is loaded before any subscriber exists, so load-time diagnostics
    // go to stderr directly.
    let mut config = deepseeknova_config::Config::load().unwrap_or_else(|e| {
        eprintln!("warning: failed to load config, using defaults: {e}");
        deepseeknova_config::Config::default()
    });

    // `--secure-defaults` 一键档：权限门控（已默认开启）保持开启 + 沙箱启用
    // （若 OS 支持）。Windows 无 OS 沙箱后端时 platform_sandbox* 回落
    // NoOpSandbox，由下方 main.rs 警告 + runtime 启动横幅明示；权限门控仍
    // 独立生效，安全姿态不因此降级。
    if cli.secure_defaults {
        config.permissions.enabled = true;
        config.sandbox.enabled = true;
        eprintln!("secure-defaults: permission gate ON, sandbox ON");
    }

    // Windows 上当前没有 OS 级沙箱后端（seatbelt/bubblewrap 均不可用），
    // `platform_sandbox*` 会回落 NoOpSandbox。即使 [sandbox] enabled=true，
    // shell 工具仍无隔离——这里必须运行时显式警告，而不是只写在 README。
    #[cfg(target_os = "windows")]
    eprintln!(
        "warning: no OS-level sandbox backend is available on Windows; \
         shell commands run without sandbox isolation. Keep permission rules \
         strict or run inside a trusted environment."
    );

    // Role-pointer routing + cost accounting. The router owns its ledger
    // (retrievable via `router.ledger()`), so no separate binding is needed.
    let model_router = Arc::new(
        deepseeknova_provider::router::ModelRouter::from_config(
            &config,
            Arc::new(deepseeknova_provider::cost::CostLedger::new()),
        )
        .unwrap_or_else(|e| {
            eprintln!("config error: {e}");
            std::process::exit(2);
        }),
    );

    // Tracing backend — exactly one of the two is installed. The telemetry
    // guard's registry carries no fmt layer, so terminal log output is
    // suppressed while OTLP export is active (known trade-off); it must be
    // held in a named binding so spans flush on exit.
    let _telemetry_guard = if config.telemetry.enabled {
        Some(deepseeknova_telemetry::TelemetryGuard::init(
            "deepseeknova",
            config.telemetry.otlp_endpoint.as_deref(),
        )?)
    } else {
        // TUI 模式下全静默：stdout 被 ratatui 独占，任何日志都破坏画面；
        // 普通 chat 模式下 INFO 级 agent/provider 日志（step、POST 等）
        // 会直接刷进对话区，降为 WARN 只保留真正需要关注的问题。
        let max_level = if tui_mode {
            LevelFilter::OFF
        } else if matches!(&cli.command, Some(Commands::Chat { tui: false, .. })) {
            LevelFilter::WARN
        } else {
            LevelFilter::INFO
        };
        // 日志一律走 stderr：stdout 保留给业务输出（chat 对话、scan 的
        // JSON 报表等），否则 `--format json` 会被 INFO 行污染无法解析。
        let subscriber = FmtSubscriber::builder()
            .with_max_level(max_level)
            .with_writer(std::io::stderr)
            .finish();
        tracing::subscriber::set_global_default(subscriber)
            .expect("setting default subscriber failed");
        None
    };

    match &cli.command {
        // ── Run (single-model or coordinator) ─────────────────────────────
        Some(Commands::Run {
            model: model_args,
            coordinator,
            prompt,
        }) => {
            let prompt_str = prompt.join(" ");
            info!(
                "run: model={:?}, max_steps={}, planner_model={:?}, prompt={prompt_str}",
                model_args.model, model_args.max_steps, coordinator.planner_model,
            );

            if let Some(ref planner_model) = coordinator.planner_model {
                // ── Coordinator mode ──────────────────────────────────────
                use deepseeknova_provider::cost::ModelRole;
                let planner_provider =
                    model_router.provider_for_model(planner_model, ModelRole::Main, None)?;
                let executor_model = coordinator
                    .executor_model
                    .clone()
                    .or_else(|| model_args.model.clone());
                let executor_provider = model_router.provider_for_maybe_model(
                    ModelRole::Task,
                    executor_model.as_deref(),
                    Some(deepseeknova_provider::factory::ReasoningEffort::High),
                )?;
                let max_nodes = coordinator.max_graph_nodes;

                let workspace_root = std::env::current_dir().unwrap_or_default();
                let security =
                    deepseeknova_runtime::build_security_context(&config, &workspace_root)?;
                let mut runner = CoordinatorRunner::new(planner_provider, executor_provider)
                    .with_max_graph_nodes(max_nodes)
                    .with_workspace_root(workspace_root.clone())
                    .with_security(security.clone());
                let permission_gate =
                    deepseeknova_runtime::permission_gate_for(&config, &workspace_root);
                if let Some(g) = &permission_gate {
                    runner = runner.with_permission_gate(g.clone());
                }

                // Delegate 动作路由：子代理走 task 指针，压缩走 compact 指针。
                // 压缩是机械摘要，按 Disabled 分类省掉 reasoning tokens。
                if config.delegate.enabled {
                    use deepseeknova_provider::factory::ReasoningEffort;
                    let task_provider =
                        model_router.provider_for(ModelRole::Task, Some(ReasoningEffort::High))?;
                    let compact_provider = model_router
                        .provider_for(ModelRole::Compact, Some(ReasoningEffort::Disabled))?;
                    runner =
                        runner.with_sub_agent_runner(deepseeknova_runtime::build_sub_agent_runner(
                            &config,
                            task_provider,
                            Some(compact_provider),
                            permission_gate
                                .as_ref()
                                .map(|g| g.deny_rules())
                                .unwrap_or(&[]),
                            permission_gate.clone(),
                            Some(security.clone()),
                            &workspace_root,
                        ));
                }

                // P4.1 Coordinator 图索引接入：注入 GraphHandle，图工具不再排除；
                // 只读工具对规划器开放（安全边界：规划器只能调用只读工具）。
                if config.graph.enabled {
                    match deepseeknova_graph::GraphIndex::open(
                        &workspace_root,
                        config.graph.max_file_size,
                    ) {
                        Ok(index) => {
                            let handle: deepseeknova_tools::GraphHandle =
                                Arc::new(std::sync::Mutex::new(index));
                            let bg = handle.clone();
                            tokio::task::spawn_blocking(move || {
                                if let Ok(mut idx) = bg.lock() {
                                    if let Err(e) = idx.refresh() {
                                        tracing::warn!("graph index refresh failed: {e}");
                                    }
                                } else {
                                    tracing::warn!("graph index lock poisoned during refresh");
                                }
                            });
                            runner = runner.with_extension(handle);
                        }
                        Err(e) => {
                            tracing::warn!("graph index unavailable, tools will degrade: {e}")
                        }
                    }
                }
                let graph_tools = ["search_code", "traverse_graph", "retrieve_entity"];
                for tool in deepseeknova_tools::all_builtin_tools() {
                    if !config.graph.enabled && graph_tools.contains(&tool.schema().name.as_str()) {
                        continue;
                    }
                    if tool.read_only() {
                        runner.register_read_only_tool(tool);
                    } else {
                        runner.register_tool(tool);
                    }
                }
                // 日常体验工具：web 搜索 / LSP 诊断（coordinator 下同样可用）。
                for tool in deepseeknova_tools::web_search_tools(&config.tools)
                    .into_iter()
                    .chain(deepseeknova_tools::lsp_diagnostics_tools(&config.tools))
                {
                    if tool.read_only() {
                        runner.register_read_only_tool(tool);
                    } else {
                        runner.register_tool(tool);
                    }
                }

                // MCP 工具：与单 Agent 路径一致，从 config.mcp_servers 发现并注册到
                // 执行器 Runner（子代理按设计不接 MCP）。graph 工具仍受上面的排除
                // 限制，待 coordinator graph wiring 落地后再补。
                for tool in deepseeknova_runtime::discover_mcp_tools(&config).await {
                    runner.register_tool(tool);
                }

                let input = RunInput {
                    prompt: prompt_str,
                    images: Vec::new(),
                    model_override: model_args.model.clone(),
                };
                let runner: Box<dyn Runner + Send> = if config.metrics.enabled {
                    Box::new(MetricsRunner::new(
                        Box::new(runner),
                        model_router.ledger(),
                        model_router.price_table(),
                        workspace_root.join(".deepseeknova").join("metrics"),
                    ))
                } else {
                    Box::new(runner)
                };
                stream_coordinator(&*runner, input).await?;
            } else {
                // ── Single-agent mode ─────────────────────────────────────
                use deepseeknova_provider::cost::ModelRole;
                let provider = model_router.provider_for_maybe_model(
                    ModelRole::Main,
                    model_args.model.as_deref(),
                    None,
                )?;
                let task_provider = model_router.provider_for(ModelRole::Task, None)?;
                let mcp_tools = deepseeknova_runtime::discover_mcp_tools(&config).await;
                let (step_quick, step_high) =
                    step_effort_providers(&model_router, &config, model_args.model.as_deref())?;
                let mut roles = deepseeknova_runtime::AgentRoleProviders::default();
                roles.task = Some(task_provider);
                roles.compact = Some(compact_provider_for(&model_router, &config)?);
                roles.review = review_provider_for(&model_router, &config)?;
                roles.step_quick = step_quick;
                roles.step_high = step_high;
                let agent = build_agent(
                    Arc::clone(&provider),
                    roles,
                    model_args.model.as_deref(),
                    &config,
                    model_args.max_steps,
                    mcp_tools,
                    &model_router,
                    Some(cli_session_label()),
                )?;
                let agent = if let Some(decider) =
                    maybe_auto_router(&model_router, &config, model_args.model.is_some())
                {
                    agent.with_auto_router(decider)
                } else {
                    agent
                };
                let agent = agent
                    // 非交互 run：Ask 无人工应答，fail-closed 拒绝而非静默放行。
                    .with_approval_responder(Arc::new(DenyApprovalResponder));

                let input = RunInput {
                    prompt: prompt_str,
                    images: Vec::new(),
                    model_override: model_args.model.clone(),
                };
                stream_events(&agent, input).await?;
            }
        }

        // ── Plan ─────────────────────────────────────────────────────────
        Some(Commands::Plan {
            model,
            coordinator,
            prompt,
        }) => {
            let prompt_str = prompt.join(" ");
            info!("plan: model={model:?}, prompt={prompt_str}");

            use deepseeknova_provider::cost::ModelRole;
            let provider =
                model_router.provider_for_maybe_model(ModelRole::Main, model.as_deref(), None)?;
            let mut plan_runner = PlanModeRunner::new(provider);

            // When coordinator flags are present, attach a Planner so the
            // output includes a structured ExecutionGraph.
            if coordinator.planner_model.is_some() {
                plan_runner = plan_runner.with_planner(Arc::new(SimplePlanner));
            }

            let input = RunInput {
                prompt: prompt_str,
                images: Vec::new(),
                model_override: model.clone(),
            };
            stream_events(&plan_runner, input).await?;
        }

        // ── Scan (matcher + optional AI investigation) ───────────────────
        Some(Commands::Scan {
            path,
            format,
            no_ai,
            severity_min,
        }) => {
            use deepseeknova_provider::cost::ModelRole;
            let workspace_root = std::env::current_dir().unwrap_or_default();
            let raw_root = std::path::PathBuf::from(path.as_deref().unwrap_or("."));
            // 逃逸企图（`..`/绝对路径/symlink）直接中止（fail-closed）；
            // 仅路径不存在等非安全失败回落归一化路径（扫描结果为空）。
            let root = resolve_scan_root(&workspace_root, &raw_root)?;
            let min = deepseeknova_scanner::rule::Severity::parse(severity_min)
                .unwrap_or(deepseeknova_scanner::rule::Severity::Low);

            info!(
                "scan: path={}, format={format}, no_ai={no_ai}",
                root.display()
            );
            let rules = deepseeknova_scanner::rule::builtin_rules();
            let mut findings = deepseeknova_scanner::scan::scan_files(&root, &rules)?;
            // severity 过滤：min 为下限，保留 severity <= min（High 序最小）。
            findings.retain(|f| f.severity <= min);

            if !*no_ai && !findings.is_empty() {
                let mcp_tools = deepseeknova_runtime::discover_mcp_tools(&config).await;
                let provider = model_router
                    .provider_for(ModelRole::Task, None)
                    .map_err(|e| anyhow::anyhow!("{e}（可用 --no-ai 跳过 AI 调查）"))?;
                // agent 一次性构建，跨 findings 复用（Agent::run_stream 每次调用克隆状态）。
                let agent = build_agent(
                    Arc::clone(&provider),
                    deepseeknova_runtime::AgentRoleProviders::default(),
                    None,
                    &config,
                    5,
                    mcp_tools,
                    &model_router,
                    Some(cli_session_label()),
                )?
                .with_approval_responder(Arc::new(DenyApprovalResponder));
                for f in &mut findings {
                    f.verdict = deepseeknova_scanner::investigate::investigate(f, &agent).await;
                }
            }

            let report = deepseeknova_scanner::report::ScanReport::new(findings);
            match format.as_str() {
                "json" => println!("{}", report.to_json()?),
                _ => println!("{}", report.to_markdown()),
            }
        }

        // ── Eval ─────────────────────────────────────────────────────────
        Some(Commands::Eval {
            path,
            format,
            require_min_score,
            require_dimension,
        }) => {
            use deepseeknova_metrics::{Scorecard, SessionStats};
            use deepseeknova_provider::cost::ModelRole;

            let cases = eval::load_cases(path)?;
            let ci = eval::CiThresholds {
                min_score: *require_min_score,
                dimension_min: require_dimension.clone(),
            };
            let provider = model_router.provider_for(ModelRole::Main, None)?;
            let mcp_tools = deepseeknova_runtime::discover_mcp_tools(&config).await;
            let agent = build_agent(
                Arc::clone(&provider),
                deepseeknova_runtime::AgentRoleProviders::default(),
                None,
                &config,
                0,
                mcp_tools,
                &model_router,
                None,
            )?;

            // eval 专属 metrics hook：把每次 run 的评分卡捕获进内存（替换
            // build_agent 挂的落盘 hook，避免 eval 评分卡污染
            // `.deepseeknova/metrics/` 质量驾驶舱聚合；quality/diagnose/
            // protocol 等其他钩子不受影响）。每 run 恰好推一张卡，用例循环
            // 按轮依次 pop。
            let captured: Arc<std::sync::Mutex<Vec<Scorecard>>> =
                Arc::new(std::sync::Mutex::new(Vec::new()));
            let capture_hook: deepseeknova_agent::MetricsHook = {
                let captured = Arc::clone(&captured);
                Arc::new(
                    move |stats: SessionStats, summary: deepseeknova_agent::QualitySummary| {
                        let session_id = summary.session_id.clone().unwrap_or_default();
                        let mut card = Scorecard::compute(
                            &session_id,
                            &stats,
                            &summary.findings,
                            summary.reflection_count,
                            summary.review_issues,
                            summary.review_passes,
                        );
                        card.fill_protocol(summary.protocol_violations, summary.phase_transitions);
                        captured
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(card);
                    },
                )
            };
            let agent = agent.with_metrics_hook(capture_hook);

            // 成本按轮差分（CostLedger 为进程级累计，before/after 之差即本轮）。
            let ledger = model_router.ledger();
            let prices = model_router.price_table();
            let mut results = Vec::new();
            for (idx, case) in cases.iter().enumerate() {
                let rounds = case.effective_rounds();
                let mut round_passed = Vec::with_capacity(rounds as usize);
                let mut round_values: Vec<eval::CaseValues> = Vec::with_capacity(rounds as usize);
                let mut total_cost: Option<f64> = None;
                let mut case_error: Option<String> = None;

                for _ in 0..rounds {
                    let cost_before = ledger.report(&prices).total_usd;
                    let run = run_eval_case(&agent, case.prompt.clone()).await;
                    let cost_after = ledger.report(&prices).total_usd;
                    if let (Some(a), Some(b)) = (cost_before, cost_after) {
                        let delta = (b - a).max(0.0);
                        total_cost = Some(total_cost.unwrap_or(0.0) + delta);
                    }
                    // 本轮评分卡（metrics hook 每 run 恰好推一张）；run 失败时
                    // hook 仍会经 Drop 兜底推一张（outcome=None），此处一并取出，
                    // 避免残留污染下一轮。
                    let card = captured.lock().unwrap_or_else(|e| e.into_inner()).pop();
                    match run {
                        Ok(output) => {
                            let values = eval::CaseValues {
                                output,
                                card,
                                cost_usd: total_cost,
                            };
                            let passed =
                                eval::evaluate_case(case, &values).iter().all(|c| c.passed);
                            round_passed.push(passed);
                            round_values.push(values);
                            if passed {
                                break;
                            }
                        }
                        Err(e) => {
                            round_passed.push(false);
                            case_error = Some(format!("{e:#}"));
                            break;
                        }
                    }
                }

                // 多轮选择：取首个通过轮，无通过轮取最后一轮。
                let (used_idx, rounds_used) = eval::select_round(&round_passed);
                let values = round_values.get(used_idx).cloned().unwrap_or_default();
                let checks = eval::evaluate_case(case, &values);
                let passed = checks.iter().all(|c| c.passed) && case_error.is_none();
                results.push(eval::EvalResult {
                    name: case.label(idx),
                    prompt: case.prompt.clone(),
                    passed,
                    checks,
                    card: values.card,
                    cost_usd: values.cost_usd,
                    rounds: rounds_used as u32,
                    output: values.output,
                    error: case_error,
                });
            }

            let summary = eval::summarize(&results, ci);
            let exit_code = eval::eval_exit_code(&summary);
            match format.as_str() {
                "json" => println!("{}", eval::render_json(&results, &summary)),
                _ => println!("{}", eval::render_markdown(&results, &summary)),
            }
            if exit_code != 0 {
                // 供 CI 门禁：1 = 条目级失败；2 = CI 门槛失败；3 = 两者。
                std::process::exit(exit_code);
            }
        }

        // ── Chat (with /new loop) ────────────────────────────────────────
        Some(Commands::Chat { model, resume, tui }) => {
            info!("chat: model={model:?}, resume={resume}, tui={tui}");
            // Compute the baseline reasoning effort from config so the
            // REPL knows what to restore when toggling thinking back on.
            let provider_cfg = resolve_provider_cfg(&config, model.as_deref());
            let baseline_effort =
                deepseeknova_provider::factory::resolve_effort(provider_cfg, None);

            // Discover MCP tools once — reused across model/effort rebuilds so
            // `/model` switching never re-spawns MCP server processes.
            let mcp_tools = deepseeknova_runtime::discover_mcp_tools(&config).await;

            // Full-screen TUI: build one agent (with a fresh shared history so
            // multi-turn context works) and hand it to the TUI runner.
            // `/model` 热切换通过 agent 工厂重建，`/cost` 走 model router。
            if *tui {
                use deepseeknova_provider::cost::ModelRole;
                use deepseeknova_provider::factory::ReasoningEffort;
                // /mcp：列出已启用 server 并做实时连接探测（短超时 spawn 检查存活）。
                let mcp_server_infos: Vec<deepseeknova_tui::McpServerInfo> = config
                    .mcp_servers
                    .iter()
                    .filter(|s| s.enabled)
                    .map(|s| deepseeknova_tui::McpServerInfo {
                        name: s.name.clone(),
                        command: s.command.clone(),
                        args: s.args.clone(),
                    })
                    .collect();
                // /undo：与 `checkpoint` 子命令同一快照库。
                let undo_controller = Arc::new(tui_undo::TuiUndoController {
                    path: std::env::current_dir()
                        .unwrap_or_default()
                        .join(&config.checkpoint.path),
                });
                let history: Arc<tokio::sync::Mutex<Vec<deepseeknova_core::Message>>> =
                    Arc::new(tokio::sync::Mutex::new(Vec::new()));
                // 会话管理：/new /sessions /resume + 回合落盘（与 REPL 同一持久化）。
                let session_controller =
                    build_chat_persistence(sessions_root(&config), history.clone(), false)
                        .await
                        .map(|p| {
                            Arc::new(TuiSessionController {
                                persist: tokio::sync::Mutex::new(p),
                            })
                                as Arc<dyn deepseeknova_tui::SessionController>
                        });
                let factory_router = Arc::clone(&model_router);
                let cfg = config.clone();
                // 生效模型名：--model 显式覆盖，否则回落配置 default_model。
                // 界面 label 与上下文分母都按它解析（曾因回落缺失显示
                // "default" 且 token 预算条永远不出现）。
                let effective_model = model.clone().or_else(|| cfg.default_model.clone());
                let context_window = resolve_provider_cfg(&cfg, effective_model.as_deref())
                    .context_window
                    .or_else(|| {
                        effective_model
                            .as_deref()
                            .and_then(|m| cfg.find_model(m))
                            .and_then(|mc| mc.context_window)
                    });
                // 预算上限作为 ctx 计量的第二分母：取 min(context_window, budget)，
                // 预算才是真实压力点（窗口配置过大时进度条不至于永远 0%）。
                let budget_window = cfg
                    .budget
                    .enabled
                    .then_some(cfg.budget.max_total_tokens as u32);
                let hist = history.clone();
                let mcp = mcp_tools;
                // 权限审批：responder 注入每次重建的 agent（/model 热切换
                // 也生效），请求接收端注入 TUI 显示确认浮层（y/n）。
                let (approval_responder, approval_rx) =
                    deepseeknova_tui::approval::approval_channel();
                let factory = move |effort: Option<ReasoningEffort>,
                                    model: Option<String>|
                      -> anyhow::Result<
                    Arc<dyn deepseeknova_core::runner::Runner>,
                > {
                    let provider = factory_router.provider_for_maybe_model(
                        ModelRole::Main,
                        model.as_deref(),
                        effort,
                    )?;
                    let task_provider = factory_router.provider_for(ModelRole::Task, effort)?;
                    let mut roles = deepseeknova_runtime::AgentRoleProviders::default();
                    roles.task = Some(task_provider);
                    roles.compact = Some(compact_provider_for(&factory_router, &cfg)?);
                    roles.review = review_provider_for(&factory_router, &cfg)?;
                    let agent = build_agent(
                        provider,
                        roles,
                        model.as_deref(),
                        &cfg,
                        0,
                        mcp.clone(),
                        &factory_router,
                        Some(cli_session_label()),
                    )?
                    .with_conversation_history(hist.clone())
                    .with_approval_responder(Arc::new(approval_responder.clone()));
                    Ok(Arc::new(agent))
                };
                let initial = factory(Some(baseline_effort), model.clone())?;
                let mut tui = deepseeknova_tui::TuiRunner::new(initial)
                    .with_model_label(effective_model.as_deref().unwrap_or("default"))
                    .with_agent_factory(factory)
                    .with_model_router(Arc::clone(&model_router))
                    .with_baseline_effort(baseline_effort)
                    .with_current_model(effective_model.clone())
                    .with_mcp_servers(mcp_server_infos)
                    .with_mcp_probe(Arc::new(mcp_probe::CliMcpProbe::default()))
                    .with_undo_controller(undo_controller)
                    .with_approval_rx(approval_rx);
                if let Some(ctrl) = session_controller {
                    tui = tui.with_session_controller(ctrl);
                }
                tui = tui.with_context_window(context_window);
                tui = tui.with_budget_window(budget_window);
                // 权限模式切换（Ctrl+P / /mode）与工作区信任确认：gate 与 agent
                // 持有同一实例（运行时已接 mode/trusted 初始状态）；TrustController
                // 委托 config TrustStore（`~/.deepseeknova/trusted.toml`）。
                let workspace_root = std::env::current_dir().unwrap_or_default();
                if let Some(g) = deepseeknova_runtime::permission_gate_for(&config, &workspace_root)
                {
                    tui = tui.with_permission_gate(g.clone());
                }
                tui = tui
                    .with_trust_controller(Arc::new(CliTrustController(
                        deepseeknova_config::TrustStore::load(),
                    )))
                    .with_workspace_root(workspace_root)
                    .with_project_rule_count(config.permissions.rules.len());
                // @ 文件补全候选：工作区文件清单（GUIDE 声称"由 CLI 注入"）。
                tui = tui.with_at_files(collect_at_files());
                tui.run().await?;
                return Ok(());
            }

            let sessions_root = sessions_root(&config);
            // Resume applies only to the first `/new` cycle.
            let mut resume_next = *resume;

            loop {
                // Persistent session memory — shared across model/effort
                // rebuilds within the same `/new` session.
                let history: Arc<tokio::sync::Mutex<Vec<deepseeknova_core::Message>>> =
                    Arc::new(tokio::sync::Mutex::new(Vec::new()));

                let persist = build_chat_persistence(
                    sessions_root.clone(),
                    Arc::clone(&history),
                    resume_next,
                )
                .await;
                resume_next = false;

                let history_clone = Arc::clone(&history);
                let cfg = &config;
                let mcp_tools = mcp_tools.clone();
                let router = Arc::clone(&model_router);
                let agent_factory =
                    move |effort: Option<deepseeknova_provider::factory::ReasoningEffort>,
                          model_name: Option<String>|
                          -> anyhow::Result<Box<dyn Runner + Send>> {
                        use deepseeknova_provider::cost::ModelRole;
                        // `/model switch <name>` 显式覆盖，仍按 Main 角色计量。
                        let provider = router.provider_for_maybe_model(
                            ModelRole::Main,
                            model_name.as_deref(),
                            effort,
                        )?;
                        let task_provider = router.provider_for(ModelRole::Task, effort)?;
                        let (step_quick, step_high) =
                            step_effort_providers(&router, cfg, model_name.as_deref())?;
                        let mut roles = deepseeknova_runtime::AgentRoleProviders::default();
                        roles.task = Some(task_provider);
                        roles.compact = Some(compact_provider_for(&router, cfg)?);
                        roles.review = review_provider_for(&router, cfg)?;
                        roles.step_quick = step_quick;
                        roles.step_high = step_high;
                        let agent = build_agent(
                            provider,
                            roles,
                            model_name.as_deref(),
                            cfg,
                            0, // no max_steps limit in chat mode
                            mcp_tools.clone(),
                            &router,
                            Some(cli_session_label()),
                        )?
                        .with_conversation_history(Arc::clone(&history_clone))
                        // REPL 交互审批：Ask 决策终端询问 y/n（权限门控默认开启，
                        // 写工具需人工裁决，与 TUI 审批浮层同语义）。
                        .with_approval_responder(chat::repl_approval_responder());
                        let agent = if let Some(decider) = maybe_auto_router(
                            &router,
                            cfg,
                            model_name.is_some() || effort.is_some(),
                        ) {
                            agent.with_auto_router(decider)
                        } else {
                            agent
                        };
                        Ok(Box::new(agent))
                    };

                let restart = chat::run_chat_repl(
                    agent_factory,
                    baseline_effort,
                    model.clone(),
                    persist,
                    Some(Arc::clone(&model_router)),
                )
                .await?;
                if !restart {
                    break;
                }
                // Drop & recreate history for the new session.
                drop(history);
                info!("restarting chat session...");
            }
        }

        Some(Commands::Serve { addr, acp, token }) => {
            info!("serve command: addr={addr}");

            use deepseeknova_provider::cost::ModelRole;
            let provider = model_router.provider_for(ModelRole::Main, None)?;
            let task_provider = model_router.provider_for(ModelRole::Task, None)?;
            let mcp_tools = deepseeknova_runtime::discover_mcp_tools(&config).await;
            let compact_provider = compact_provider_for(&model_router, &config)?;
            let review_provider = review_provider_for(&model_router, &config)?;

            if *acp {
                // ACP stdio 模式：每个 session/new 用其 cwd 作为工作区边界重建
                // agent，并挂一份共享会话历史，使连续 prompt 保持上下文。Ask
                // 权限在无权限 RPC 的情况下 fail-closed 拒绝。
                let cfg = config.clone();
                let acp_tools = mcp_tools.clone();
                let factory: deepseeknova_serve::AcpRunnerFactory =
                    Arc::new(move |workspace_root, history| {
                        let mut roles = deepseeknova_runtime::AgentRoleProviders::default();
                        roles.task = Some(Arc::clone(&task_provider));
                        roles.compact = Some(Arc::clone(&compact_provider));
                        roles.review = review_provider.clone();
                        let agent = build_agent_in(
                            workspace_root,
                            Arc::clone(&provider),
                            roles,
                            None,
                            &cfg,
                            0,
                            acp_tools.clone(),
                            &model_router,
                            None,
                        )?;
                        let agent = agent.with_conversation_history(history);
                        let agent =
                            if let Some(decider) = maybe_auto_router(&model_router, &cfg, false) {
                                agent.with_auto_router(decider)
                            } else {
                                agent
                            };
                        let agent = agent.with_approval_responder(Arc::new(DenyApprovalResponder));
                        Ok(Arc::new(agent) as Arc<dyn Runner>)
                    });
                info!("serve: acp stdio mode");
                return deepseeknova_serve::serve_acp(factory).await;
            }

            // Share a pending-approvals map between the agent's approval
            // responder and the server's POST /v1/approval route so the gate's
            // `Ask` decisions can be answered over HTTP.
            let pending = deepseeknova_serve::new_pending_approvals();
            let responder: Arc<dyn deepseeknova_core::runner::ApprovalResponder> = Arc::new(
                deepseeknova_serve::ServerApprovalResponder::new(pending.clone()),
            );
            // 会话工厂在 build_agent 之后仍要复用 provider/tools，先各留一份。
            let sess_mcp_tools = mcp_tools.clone();
            let sess_provider = Arc::clone(&provider);
            let sess_task_provider = task_provider.clone();
            let sess_compact_provider = compact_provider.clone();
            let sess_review_provider = review_provider.clone();
            let mut roles = deepseeknova_runtime::AgentRoleProviders::default();
            roles.task = Some(task_provider);
            roles.compact = Some(compact_provider);
            roles.review = review_provider;
            let agent = build_agent(
                Arc::clone(&provider),
                roles,
                None,
                &config,
                0,
                mcp_tools,
                &model_router,
                None, // serve：每次 run 由 Agent 生成唯一会话标注
            )?;
            let agent = if let Some(decider) = maybe_auto_router(&model_router, &config, false) {
                agent.with_auto_router(decider)
            } else {
                agent
            };
            let agent = agent.with_approval_responder(responder);
            let runner: Arc<dyn Runner> = Arc::new(agent);

            let workspace_root = std::env::current_dir().unwrap_or_default();

            // 多轮会话端点（/v1/sessions*）：与 ACP 相同的 per-session runner
            // 工厂，但工作区固定为进程启动目录。会话存到与 CLI/TUI 相同的
            // JSONL 目录（[session] root 或 ~/.deepseeknova/sessions），
            // 桌面端与终端看到同一批会话。
            let sessions = sessions_root(&config).map(|dir| {
                let cfg = config.clone();
                let sess_router = model_router.clone();
                let sess_workspace_root = workspace_root.clone();
                let sess_pending = pending.clone();
                let factory: deepseeknova_serve::SessionRunnerFactory = Arc::new(
                    move |history: Arc<tokio::sync::Mutex<Vec<deepseeknova_core::Message>>>| {
                        let mut roles = deepseeknova_runtime::AgentRoleProviders::default();
                        roles.task = Some(Arc::clone(&sess_task_provider));
                        roles.compact = Some(Arc::clone(&sess_compact_provider));
                        roles.review = sess_review_provider.clone();
                        let agent = build_agent_in(
                            sess_workspace_root.clone(),
                            Arc::clone(&sess_provider),
                            roles,
                            None,
                            &cfg,
                            0,
                            sess_mcp_tools.clone(),
                            &sess_router,
                            None,
                        )?;
                        let agent = agent.with_conversation_history(history);
                        let agent =
                            if let Some(decider) = maybe_auto_router(&sess_router, &cfg, false) {
                                agent.with_auto_router(decider)
                            } else {
                                agent
                            };
                        let agent = agent.with_approval_responder(Arc::new(
                            deepseeknova_serve::ServerApprovalResponder::new(sess_pending.clone()),
                        ));
                        Ok(Arc::new(agent) as Arc<dyn Runner>)
                    },
                );
                match deepseeknova_serve::SessionManager::open(dir, factory) {
                    Ok(manager) => Some(Arc::new(manager)),
                    Err(e) => {
                        tracing::warn!("session endpoints disabled: {e}");
                        None
                    }
                }
            });
            let sessions = sessions.flatten();

            let metrics_dir = workspace_root.join(".deepseeknova").join("metrics");
            let mut server = deepseeknova_serve::Server::with_pending(runner, pending)
                .with_metrics_dir(metrics_dir)
                .with_runs_dir(workspace_root.join(".deepseeknova").join("runs"))
                .with_auth_token(token.clone());
            if let Some(sessions) = sessions {
                server = server.with_sessions(sessions);
            }
            server.serve(addr).await?;
        }

        Some(Commands::Setup { local }) => {
            info!("setup: local={local}");
            setup::run_setup_wizard(*local).await?;
        }

        Some(Commands::Config) => {
            println!("{:#?}", config);
        }

        // ── Memory (审查/检索/删除/统计) ─────────────────────────────────
        Some(Commands::Memory { action }) => {
            use deepseeknova_core::memory::store::MemoryCategory;
            let db = std::env::current_dir()
                .unwrap_or_default()
                .join(&config.memory.db_path);
            let memory_embedder =
                deepseeknova_provider::embeddings::try_memory_embedder(&config.memory);
            let memory_embed_model = memory_embedder
                .as_ref()
                .map(|_| config.memory.embed_model.clone());
            let engine = deepseeknova_core::memory::engine::MemoryEngine::open_with_embedder(
                &db,
                config.memory.redact_secrets,
                memory_embedder,
                memory_embed_model,
            )?;
            match action {
                cli::MemoryAction::List { category, limit } => {
                    let cat = match category.as_str() {
                        "skill" => MemoryCategory::Skill,
                        "user_profile" => MemoryCategory::UserProfile,
                        _ => MemoryCategory::Task,
                    };
                    for e in engine.list(cat)?.into_iter().take(*limit) {
                        let preview: String = e.content.chars().take(100).collect();
                        println!("[{}] ({}) {}", e.id, e.source, preview);
                    }
                }
                cli::MemoryAction::Search { query } => {
                    let q = query.join(" ");
                    let hits =
                        engine.recall_with_weight(&q, 10, config.memory.rank_lifecycle_weight)?;
                    for (i, r) in hits.iter().enumerate() {
                        let preview: String = r.entry.content.chars().take(120).collect();
                        println!("{}. [{}] {}", i + 1, r.entry.id, preview);
                    }
                }
                cli::MemoryAction::Forget { id } => {
                    println!(
                        "{}",
                        if engine.forget(id)? {
                            "removed"
                        } else {
                            "not found"
                        }
                    );
                }
                cli::MemoryAction::Stats => {
                    let s = engine.stats()?;
                    let stages = s
                        .stage_counts
                        .iter()
                        .map(|(k, v)| format!("{k}:{v}"))
                        .collect::<Vec<_>>()
                        .join(",");
                    println!(
                        "total={} embedded={} recall_hit_rate={:.2} reinforce_ratio={:.2} stages={} archived={}",
                        s.total, s.embedded, s.recall_hit_rate, s.reinforce_ratio, stages, s.archived
                    );
                }
                cli::MemoryAction::EmbedBackfill => {
                    let (attempted, ok) = engine.backfill_embeddings()?;
                    println!("embed-backfill: attempted={attempted} ok={ok}");
                }
                cli::MemoryAction::Cleanup => {
                    let (decayed, deleted) =
                        engine.cleanup(config.memory.decay_rate, config.memory.archive_ttl_days)?;
                    println!("cleanup: decayed={decayed} deleted={deleted}");
                }
            }
        }

        // ── Checkpoint（写前快照 + 回滚，A1）────────────────────────────
        Some(Commands::Checkpoint { action }) => {
            use deepseeknova_checkpoint::CheckpointManager;
            let workspace_root = std::env::current_dir().unwrap_or_default();
            let path = workspace_root.join(&config.checkpoint.path);
            match action {
                cli::CheckpointAction::List => {
                    let ck = CheckpointManager::load_from(&path)?;
                    if ck.is_empty() {
                        println!("no checkpoints (path: {})", path.display());
                    }
                    for (snap, clean) in ck.verify().await? {
                        let status = if clean { "unchanged" } else { "modified" };
                        println!(
                            "{} [{}] {} ({})",
                            if clean { "✓" } else { "✗" },
                            status,
                            snap.path.display(),
                            &snap.hash[..8.min(snap.hash.len())]
                        );
                    }
                }
                cli::CheckpointAction::Rollback { all } => {
                    let mut ck = CheckpointManager::load_from(&path)?;
                    if *all {
                        let n = ck.rollback_all().await?;
                        println!("rolled back {n} snapshot(s)");
                    } else if let Some((p, h)) = ck.rollback().await? {
                        println!(
                            "rolled back {} (hash {})",
                            p.display(),
                            &h[..8.min(h.len())]
                        );
                    } else {
                        println!("no checkpoints to roll back");
                    }
                }
                cli::CheckpointAction::Clear => {
                    let mut ck = CheckpointManager::load_from(&path)?;
                    let n = ck.len();
                    ck.clear();
                    println!("cleared {n} snapshot(s)");
                }
            }
        }

        // ── Artifacts（项目后置产出，A2）────────────────────────────────
        Some(Commands::Artifacts { action }) => {
            use deepseeknova_core::artifacts::cards::{CardGenerator, KnowledgeCard};
            use deepseeknova_core::artifacts::wiki::{ProjectSummary, WikiConfig, WikiGenerator};
            match action {
                cli::ArtifactsAction::Wiki {
                    out,
                    project,
                    summary,
                } => {
                    let name = project.clone().unwrap_or_else(|| {
                        std::env::current_dir()
                            .ok()
                            .and_then(|d| d.file_name().map(|s| s.to_string_lossy().into_owned()))
                            .unwrap_or_else(|| "project".to_string())
                    });
                    let mut gen = WikiGenerator::new(WikiConfig {
                        output_dir: std::path::PathBuf::from(out),
                        ..Default::default()
                    });
                    gen.add_home_page(&ProjectSummary {
                        name,
                        description: summary.clone().unwrap_or_default(),
                        modules: vec![],
                        key_decisions: vec![],
                        metrics: vec![],
                    });
                    for p in gen.generate()? {
                        println!("{}", p.display());
                    }
                }
                cli::ArtifactsAction::Cards {
                    out,
                    title,
                    insight,
                    tags,
                    source,
                } => {
                    let mut gen = CardGenerator::new(out);
                    gen.add_card(KnowledgeCard {
                        id: format!("card-{}", chrono::Utc::now().timestamp()),
                        title: title.clone(),
                        tags: tags.clone(),
                        created: chrono::Utc::now().format("%Y-%m-%d").to_string(),
                        source: source.clone().unwrap_or_else(|| "cli".to_string()),
                        context: String::new(),
                        key_insight: insight.clone(),
                        code_example: None,
                        related: vec![],
                    });
                    for p in gen.generate()? {
                        println!("{}", p.display());
                    }
                }
            }
        }

        Some(Commands::Init { legacy }) => {
            info!("init command (legacy={legacy})");
            init::run_init(*legacy).await?;
        }

        None => {
            info!("no command provided — starting interactive chat");
            // Resolve baseline effort from the default provider config.
            let provider_cfg = resolve_provider_cfg(&config, None);
            let baseline_effort =
                deepseeknova_provider::factory::resolve_effort(provider_cfg, None);

            // Discover MCP tools once (see Chat branch for rationale).
            let mcp_tools = deepseeknova_runtime::discover_mcp_tools(&config).await;
            let sessions_root = sessions_root(&config);

            loop {
                let history: Arc<tokio::sync::Mutex<Vec<deepseeknova_core::Message>>> =
                    Arc::new(tokio::sync::Mutex::new(Vec::new()));

                let persist =
                    build_chat_persistence(sessions_root.clone(), Arc::clone(&history), false)
                        .await;

                let history_clone = Arc::clone(&history);
                let cfg = &config;
                let mcp_tools = mcp_tools.clone();
                let router = Arc::clone(&model_router);
                let agent_factory =
                    move |effort: Option<deepseeknova_provider::factory::ReasoningEffort>,
                          model_name: Option<String>|
                          -> anyhow::Result<Box<dyn Runner + Send>> {
                        use deepseeknova_provider::cost::ModelRole;
                        // `/model switch <name>` 显式覆盖，仍按 Main 角色计量。
                        let provider = router.provider_for_maybe_model(
                            ModelRole::Main,
                            model_name.as_deref(),
                            effort,
                        )?;
                        let task_provider = router.provider_for(ModelRole::Task, effort)?;
                        let mut roles = deepseeknova_runtime::AgentRoleProviders::default();
                        roles.task = Some(task_provider);
                        roles.compact = Some(compact_provider_for(&router, cfg)?);
                        roles.review = review_provider_for(&router, cfg)?;
                        let agent = build_agent(
                            provider,
                            roles,
                            model_name.as_deref(),
                            cfg,
                            0,
                            mcp_tools.clone(),
                            &router,
                            Some(cli_session_label()),
                        )?
                        .with_conversation_history(Arc::clone(&history_clone))
                        // 无子命令 = 交互 REPL：同 chat 路径注入交互审批。
                        .with_approval_responder(chat::repl_approval_responder());
                        let agent = if let Some(decider) = maybe_auto_router(
                            &router,
                            cfg,
                            model_name.is_some() || effort.is_some(),
                        ) {
                            agent.with_auto_router(decider)
                        } else {
                            agent
                        };
                        Ok(Box::new(agent))
                    };

                let restart = chat::run_chat_repl(
                    agent_factory,
                    baseline_effort,
                    None,
                    persist,
                    Some(Arc::clone(&model_router)),
                )
                .await?;
                if !restart {
                    break;
                }
                drop(history);
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// 收集工作区文件路径，供 TUI 的 `@` 文件补全注入候选。
///
/// 递归扫 cwd，跳过常见噪声目录（.git/target/node_modules/.deepseeknova 等）
/// 与隐藏条目；上限 `MAX_AT_FILES` 条防止大仓库拖慢启动。返回相对路径。
fn collect_at_files() -> Vec<String> {
    const MAX_AT_FILES: usize = 500;
    const SKIP_DIRS: &[&str] = &[
        ".git",
        ".svn",
        "target",
        "node_modules",
        ".deepseeknova",
        ".cargo",
        ".cache",
        "dist",
        "build",
    ];
    let mut out = Vec::new();
    let mut stack = vec![std::path::PathBuf::from(".")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // 目录 symlink 一律跳过：`is_dir()` 会跟随符号链接，指向祖先或
            // 自身的链接会形成环导致无限递归，指向大目录（如 node_modules）
            // 则把无关文件扫进来。文件 symlink 同理不跟随（引用价值低）。
            if entry.file_type().is_ok_and(|ft| ft.is_symlink()) {
                continue;
            }
            if name.starts_with('.') && path.is_dir() {
                continue;
            }
            if path.is_dir() {
                if SKIP_DIRS.contains(&name.as_ref()) {
                    continue;
                }
                stack.push(path);
            } else if out.len() < MAX_AT_FILES {
                out.push(path.to_string_lossy().replace("./", ""));
            }
        }
    }
    out
}

/// Resolve a ProviderConfig for a given model name (or the default).
fn resolve_provider_cfg<'a>(
    config: &'a deepseeknova_config::Config,
    model: Option<&str>,
) -> &'a deepseeknova_config::ProviderConfig {
    if let Some(model_name) = model {
        config
            .resolve_provider_for_model(model_name)
            .unwrap_or_else(|| &config.providers[0])
    } else {
        &config.providers[0]
    }
}

/// Compact 覆盖模型判定：指针优先；指针未设而 B2 的 agent.compact_model
/// 非空时，以该模型为显式覆盖（经 router 构建，照样计量）。
fn compact_override_model(config: &deepseeknova_config::Config) -> Option<&str> {
    if config.model_pointers.compact.is_none() && !config.agent.compact_model.is_empty() {
        Some(config.agent.compact_model.as_str())
    } else {
        None
    }
}

/// Compact 角色 provider（Agent L3 压缩用）。L3 摘要是机械任务，按 Disabled
/// 分类省 reasoning tokens（与 coordinator compact 决策一致）。
fn compact_provider_for(
    router: &deepseeknova_provider::router::ModelRouter,
    config: &deepseeknova_config::Config,
) -> anyhow::Result<Arc<dyn deepseeknova_provider::Provider>> {
    router.provider_for_maybe_model(
        deepseeknova_provider::cost::ModelRole::Compact,
        compact_override_model(config),
        Some(deepseeknova_provider::factory::ReasoningEffort::Disabled),
    )
}

/// P2.1 每步 effort 路由的 quick/high provider（未启用时返回 (None, None)）。
type EffortProviderPair = (
    Option<Arc<dyn deepseeknova_provider::Provider>>,
    Option<Arc<dyn deepseeknova_provider::Provider>>,
);

fn step_effort_providers(
    router: &deepseeknova_provider::router::ModelRouter,
    config: &deepseeknova_config::Config,
    model: Option<&str>,
) -> anyhow::Result<EffortProviderPair> {
    use deepseeknova_provider::cost::ModelRole;
    use deepseeknova_provider::factory::ReasoningEffort;
    if !config.agent.step_effort_routing {
        return Ok((None, None));
    }
    let quick =
        router.provider_for_maybe_model(ModelRole::Main, model, Some(ReasoningEffort::Disabled))?;
    let high =
        router.provider_for_maybe_model(ModelRole::Main, model, Some(ReasoningEffort::High))?;
    Ok((Some(quick), Some(high)))
}

/// Review 角色 provider（B3 完成前自审门禁用）。review 关闭时不构建（避免
/// 无关路径因 quick 指针的 API key 缺失阻断构建）；review_model 非空时按名
/// 经 router 构建（照样计量）；空则走 quick 指针（门禁属快速操作类，未设时
/// 回落 main 指针）。评审判定受益于 reasoning，不强制降档 effort。
fn review_provider_for(
    router: &deepseeknova_provider::router::ModelRouter,
    config: &deepseeknova_config::Config,
) -> anyhow::Result<Option<Arc<dyn deepseeknova_provider::Provider>>> {
    if !config.review.enabled {
        return Ok(None);
    }
    let override_model = if config.review.review_model.is_empty() {
        None
    } else {
        Some(config.review.review_model.as_str())
    };
    Ok(Some(router.provider_for_maybe_model(
        deepseeknova_provider::cost::ModelRole::Quick,
        override_model,
        None,
    )?))
}

/// Run one eval case and return the agent's final text (streaming deltas are
/// accumulated; the `Done` output wins if present).
async fn run_eval_case(runner: &dyn Runner, prompt: String) -> anyhow::Result<String> {
    let input = RunInput {
        prompt,
        images: Vec::new(),
        model_override: None,
    };
    let mut stream = runner.run_stream(input).await?;
    let mut text = String::new();
    while let Some(event) = stream.next().await {
        match event? {
            RunEvent::TextDelta(t) => text.push_str(&t),
            RunEvent::Done(out) => text = out.text,
            _ => {}
        }
    }
    Ok(text)
}

/// Build the per-run auto model+thinking decider when enabled and the user did
/// not explicitly pick a model/effort (`--model` / `/model switch` / 显式
/// effort 绕过 auto 模式，显式选择永远优先)。`None` = 不启用。
fn maybe_auto_router(
    router: &Arc<deepseeknova_provider::router::ModelRouter>,
    config: &deepseeknova_config::Config,
    explicit_override: bool,
) -> Option<Arc<dyn deepseeknova_provider::auto::AutoRouteDecider>> {
    if config.agent.auto_route && !explicit_override {
        Some(Arc::new(deepseeknova_provider::auto::ModelAutoRouter::new(
            Arc::clone(router),
            config.agent.auto_router_model.clone(),
            config.agent.auto_router_max_chars,
        )))
    } else {
        None
    }
}

/// Build an agent with built-in tools registered, plus any `extra_tools`
/// (e.g. MCP tools discovered via [`deepseeknova_runtime::discover_mcp_tools`]).
/// `roles` routes the delegation engine (sub-agents) to the Task-role model
/// and Agent L3 compaction to the Compact-role model when supplied; unset
/// roles fall back to the main provider.
#[allow(clippy::too_many_arguments)]
fn build_agent(
    provider: Arc<dyn deepseeknova_provider::Provider>,
    roles: deepseeknova_runtime::AgentRoleProviders,
    _model: Option<&str>,
    config: &deepseeknova_config::Config,
    max_steps: usize,
    extra_tools: Vec<Arc<dyn deepseeknova_core::Tool>>,
    router: &deepseeknova_provider::router::ModelRouter,
    session_label: Option<String>,
) -> anyhow::Result<deepseeknova_agent::Agent> {
    build_agent_in(
        std::env::current_dir().unwrap_or_default(),
        provider,
        roles,
        _model,
        config,
        max_steps,
        extra_tools,
        router,
        session_label,
    )
}

/// `build_agent` 的显式工作区变体：ACP `session/new` 的 `cwd` 需要作为会话的
/// 文件系统边界，而不是进程启动目录。
#[allow(clippy::too_many_arguments)]
fn build_agent_in(
    workspace_root: PathBuf,
    provider: Arc<dyn deepseeknova_provider::Provider>,
    roles: deepseeknova_runtime::AgentRoleProviders,
    _model: Option<&str>,
    config: &deepseeknova_config::Config,
    max_steps: usize,
    extra_tools: Vec<Arc<dyn deepseeknova_core::Tool>>,
    router: &deepseeknova_provider::router::ModelRouter,
    session_label: Option<String>,
) -> anyhow::Result<deepseeknova_agent::Agent> {
    let metrics_dir = workspace_root.join(".deepseeknova").join("metrics");
    // 会话技能名收集器（P 任务 2，spec §13 #9）：builder 注入侧把实际注入
    // prompt 的技能名写入，会话结束由 attach_metrics_hook_with_fitness 消费
    // 做 fitness record_use/record_result。
    let session_skills: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    // Delegate to the shared runtime builder (security + sandbox + permission
    // gate wiring lives in one place). CLI 非交互路径由各调用方显式注入
    // DenyApprovalResponder；即便漏挂，agent 侧的 Ask 兜底也已默认
    // fail-closed（permissions.ask_without_responder=deny），不会静默放行。
    let agent = deepseeknova_runtime::build_agent_with_role_providers(
        config,
        workspace_root.clone(),
        provider,
        roles,
        max_steps,
        None,
        extra_tools,
        Some(session_skills.clone()),
    )?;
    // 任务质量闭环装配：metrics（报告 + 评分卡落盘）、quality（ToolHook 链 +
    // 写后策略评估，`[quality] enabled` 开关）、diagnose（失败诊断报告，与
    // metrics 同目录 `diagnose/` 子目录）。诊断 dir 与 metrics dir 同源。
    // 协议增强（`[protocol] enabled`）：metrics 侧顺带 fitness 记录（会话技能
    // 名集合由注入侧回填，见上）；diagnose 侧顺带失败模式聚类；会话启动前
    // 注入历史失败模式（≤3 条）到首轮 system prompt。
    let agent = deepseeknova_runtime::attach_metrics_hook_with_fitness(
        agent,
        config,
        deepseeknova_runtime::MetricsSink {
            ledger: router.ledger(),
            prices: router.price_table(),
            dir: metrics_dir.clone(),
        },
        &workspace_root,
        // 会话技能名集合：builder 注入侧已回填实际注入的技能名；空集合 =
        // 本会话无注入技能，fitness 优雅跳过（不 warn）。
        session_skills,
    );
    let agent = deepseeknova_runtime::attach_quality_hook(agent, config);
    let agent = deepseeknova_runtime::attach_diagnose_hook_with_ingest(
        agent,
        metrics_dir,
        Some(config),
        &workspace_root,
    );
    let agent =
        deepseeknova_runtime::attach_failure_pattern_injection(agent, config, &workspace_root);
    // 协议增强：协议门控装配（`[protocol] enabled` 时挂内置四门 + 对抗审查
    // 开关）。置于回灌之后（回灌只改 prompt，门控挂 agent 配置，顺序无耦合）。
    let agent = deepseeknova_runtime::attach_protocol_gates(agent, config, &workspace_root);
    // F11：会话标注注入（仅单次 run 模式）。Paused 事件的 session_id 与
    // 诊断报告/评分卡文件名同源；label 仅含 `[A-Za-z0-9_-]`，可直接用作
    // serve `/v1/sessions/{id}/...` 路径 id。serve 模式传 None，由 Agent
    // 每次 run 生成唯一标注，避免多会话共用同一 id 互相覆盖。
    Ok(match session_label {
        Some(label) => agent.with_session_label(label),
        None => agent,
    })
}

/// 生成 CLI run 的会话标注（`session-<ts>-<seq>`）。与 store 侧 `chat-<ts>`
/// 风格对齐；仅含 ASCII 字母数字与 `_`/`-`（serve 端点 id 白名单）。序号
/// 保证同一秒内多次 build_agent（chat 模式每轮重建）也拿到唯一 id。
fn cli_session_label() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "session-{}-{}",
        chrono::Utc::now().format("%Y%m%d-%H%M%S"),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// Directory where chat sessions are persisted, driven by `[session]` config:
/// `enabled = false` disables persistence entirely; empty `root` keeps the
/// pre-B2 default `~/.deepseeknova/sessions`; non-empty `root` is used as-is.
fn sessions_root(config: &deepseeknova_config::Config) -> Option<std::path::PathBuf> {
    if !config.session.enabled {
        return None;
    }
    if config.session.root.is_empty() {
        dirs::home_dir().map(|h| h.join(".deepseeknova").join("sessions"))
    } else {
        Some(std::path::PathBuf::from(&config.session.root))
    }
}

/// Build the chat persistence context for one session.
///
/// Returns `None` (persistence disabled) when no root is available or the
/// store can't be opened — chat still works, it just isn't recorded. When
/// `resume` is set, the newest saved session is loaded into `history` and
/// becomes the active session; any failure falls back to a fresh session.
async fn build_chat_persistence(
    root: Option<std::path::PathBuf>,
    history: Arc<tokio::sync::Mutex<Vec<deepseeknova_core::Message>>>,
    resume: bool,
) -> Option<chat::ChatPersistence> {
    let store = match deepseeknova_store::SessionStore::new(root?) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("session store unavailable, persistence disabled: {e}");
            return None;
        }
    };

    let mut session_id = deepseeknova_store::new_session_id();
    let mut turn = 0u64;

    if resume {
        // Session ids sort lexicographically by time, so max() is the newest.
        let latest = store
            .list_sessions()
            .ok()
            .and_then(|ids| ids.into_iter().max());
        match latest {
            Some(id) => match store.load(&id) {
                Ok(turns) if !turns.is_empty() => {
                    let mut hist = history.lock().await;
                    for t in &turns {
                        for m in &t.messages {
                            hist.push(m.into());
                        }
                    }
                    let restored = hist.len();
                    drop(hist);
                    session_id = id.clone();
                    turn = turns.len() as u64;
                    println!("resumed session '{id}' — {restored} messages restored");
                }
                Ok(_) => tracing::warn!("latest session '{id}' is empty; starting fresh"),
                Err(e) => {
                    tracing::warn!("failed to load session '{id}' for resume: {e}; starting fresh")
                }
            },
            None => tracing::warn!("no saved session to resume; starting fresh"),
        }
    }

    Some(chat::ChatPersistence {
        store,
        session_id,
        turn,
        history,
    })
}

/// TUI 会话控制器：把 REPL 的 ChatPersistence 适配到 TUI 的 `/new` `/sessions`
/// `/resume` 与回合落盘（TUI crate 不依赖 CLI 类型）。
struct TuiSessionController {
    persist: tokio::sync::Mutex<chat::ChatPersistence>,
}

/// TUI 工作区信任控制器：委托 config `TrustStore`（`~/.deepseeknova/trusted.toml`）。
/// 首进带项目层权限规则的工作区时，TUI 弹信任确认浮层，`trust`/`untrust` 落盘
/// 并切换共享 PermissionGate 的 trusted 状态（未信任则项目层 allow 降级 ask）。
#[derive(Clone)]
struct CliTrustController(deepseeknova_config::TrustStore);

impl deepseeknova_tui::TrustController for CliTrustController {
    fn is_trusted(&self, root: &std::path::Path) -> bool {
        self.0.is_trusted(root)
    }

    fn trust(&self, root: &std::path::Path) -> anyhow::Result<()> {
        let mut store = self.0.clone();
        store.trust(root);
        store.save()
    }

    fn untrust(&self, root: &std::path::Path) -> anyhow::Result<()> {
        let mut store = self.0.clone();
        store.untrust(root);
        store.save()
    }
}

#[async_trait::async_trait]
impl deepseeknova_tui::SessionController for TuiSessionController {
    async fn new_session(&self) -> anyhow::Result<()> {
        let mut p = self.persist.lock().await;
        p.history.lock().await.clear();
        p.session_id = deepseeknova_store::new_session_id();
        p.turn = 0;
        Ok(())
    }

    async fn list_sessions(&self) -> anyhow::Result<Vec<deepseeknova_tui::SessionMeta>> {
        let p = self.persist.lock().await;
        let metas = p.store.list_sessions_with_preview()?;
        Ok(metas
            .into_iter()
            .map(|(id, preview)| deepseeknova_tui::SessionMeta { id, preview })
            .collect())
    }

    async fn current_session(&self) -> Option<String> {
        let p = self.persist.lock().await;
        Some(p.session_id.clone())
    }

    async fn resume(&self, id: &str) -> anyhow::Result<Vec<deepseeknova_tui::ResumedLine>> {
        let mut p = self.persist.lock().await;
        let turns = p.store.load(id)?;
        if turns.is_empty() {
            anyhow::bail!("session '{id}' is empty or does not exist");
        }
        let mut hist = p.history.lock().await;
        hist.clear();
        let mut restored = Vec::new();
        for t in &turns {
            for m in &t.messages {
                hist.push(m.into());
                // 工具调用/结果是执行细节：不灌入恢复后的对话面板，
                // 避免大段工具输出淹没用户与助手正文。
                if m.role == "tool" {
                    continue;
                }
                let role = match m.role.as_str() {
                    "assistant" => deepseeknova_tui::ResumedRole::Assistant,
                    "system" => deepseeknova_tui::ResumedRole::System,
                    _ => deepseeknova_tui::ResumedRole::User,
                };
                restored.push(deepseeknova_tui::ResumedLine {
                    role,
                    text: m.content.clone(),
                });
            }
        }
        drop(hist);
        p.session_id = id.to_string();
        p.turn = turns.len() as u64;
        Ok(restored)
    }

    async fn record_turn(
        &self,
        prompt: &str,
        output_text: &str,
        model: Option<String>,
    ) -> anyhow::Result<()> {
        let mut p = self.persist.lock().await;
        p.turn += 1;
        let messages = vec![
            deepseeknova_core::Message {
                role: deepseeknova_core::Role::User,
                content: prompt.to_string(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            deepseeknova_core::Message {
                role: deepseeknova_core::Role::Assistant,
                content: output_text.to_string(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
        ];
        let stored_input = RunInput {
            prompt: prompt.to_string(),
            images: Vec::new(),
            model_override: model,
        };
        let stored_turn = deepseeknova_store::SessionStore::build_turn(
            &stored_input,
            p.turn,
            messages,
            Some(deepseeknova_store::StoredOutput {
                text: output_text.to_string(),
                tool_calls: Vec::new(),
            }),
        );
        p.store.append(&p.session_id, &stored_turn)?;
        Ok(())
    }
}

/// Stream events from any [`Runner`] to stdout in a consistent format.
async fn stream_events(runner: &dyn Runner, input: RunInput) -> anyhow::Result<()> {
    let mut stream = runner.run_stream(input).await?;
    while let Some(event) = stream.next().await {
        match event? {
            deepseeknova_core::RunEvent::TextDelta(text) => {
                print!("{text}");
            }
            deepseeknova_core::RunEvent::ToolCallStart { id, name } => {
                println!("\n🔧 {name} (call {id})...");
            }
            deepseeknova_core::RunEvent::ToolCallEnd {
                name: _, arguments, ..
            } => {
                println!("   args: {arguments}");
            }
            deepseeknova_core::RunEvent::Usage(u) => {
                info!("tokens: {}/{}", u.prompt_tokens, u.completion_tokens);
            }
            deepseeknova_core::RunEvent::TurnComplete => {
                println!();
            }
            deepseeknova_core::RunEvent::Done(output) => {
                println!("\n--- done ---");
                if !output.text.is_empty() {
                    println!("{}", output.text);
                }
            }
            deepseeknova_core::RunEvent::Paused { reason, session_id } => {
                eprintln!("\n⏸ paused: {reason}");
                match session_id {
                    Some(id) => {
                        eprintln!("resume with: deepseeknova chat --resume   (session {id})")
                    }
                    None => eprintln!("resume with: deepseeknova chat --resume"),
                }
                // 非交互（CI/脚本）可判定的专用退出码：3 = paused。
                std::process::exit(3);
            }
            _ => {}
        }
    }
    Ok(())
}

/// Stream from a [`CoordinatorRunner`] — uses plan-aware display labels.
async fn stream_coordinator(runner: &dyn Runner, input: RunInput) -> anyhow::Result<()> {
    let mut stream = runner.run_stream(input).await?;
    while let Some(event) = stream.next().await {
        match event? {
            deepseeknova_core::RunEvent::TextDelta(text) => {
                print!("{text}");
            }
            deepseeknova_core::RunEvent::ToolCallStart { id, name } => {
                println!("\n⚡ {name} (call {id})...");
            }
            deepseeknova_core::RunEvent::ToolCallEnd {
                name: _, arguments, ..
            } => {
                println!("   args: {arguments}");
            }
            deepseeknova_core::RunEvent::ToolResult { call_id, result } => {
                let truncated = truncate_str(&result, 300);
                println!("   → {truncated}");
                let _ = call_id;
            }
            deepseeknova_core::RunEvent::Usage(u) => {
                info!("tokens: {}/{}", u.prompt_tokens, u.completion_tokens);
            }
            deepseeknova_core::RunEvent::Done(output) => {
                println!("\n--- coordinator done ---");
                if !output.text.is_empty() {
                    println!("{}", output.text);
                }
            }
            deepseeknova_core::RunEvent::ReasoningDelta { text, .. } => {
                // Show reasoning in dim text for coordinator planning.
                print!("\x1b[2m{text}\x1b[0m");
            }
            deepseeknova_core::RunEvent::Paused { reason, session_id } => {
                eprintln!("\n⏸ paused: {reason}");
                match session_id {
                    Some(id) => {
                        eprintln!("resume with: deepseeknova chat --resume   (session {id})")
                    }
                    None => eprintln!("resume with: deepseeknova chat --resume"),
                }
                // 非交互（CI/脚本）可判定的专用退出码：3 = paused。
                std::process::exit(3);
            }
            _ => {}
        }
    }
    Ok(())
}

/// 非交互 CLI 的审批应答：`Ask` 一律拒绝（fail-closed），避免无人工确认时
/// 静默放行写操作。交互面（TUI/HTTP）使用各自的真实应答器。
struct DenyApprovalResponder;

#[async_trait]
impl deepseeknova_core::runner::ApprovalResponder for DenyApprovalResponder {
    async fn request(&self, _id: &str, _title: &str, _description: Option<&str>) -> bool {
        false
    }
}

/// Coordinator 路径的会话指标包装器：CoordinatorRunner 本身不挂 metrics
/// hook，这里在 CLI 侧补齐 SessionMetrics 落盘（执行面 + 成本面）。
struct MetricsRunner {
    inner: Box<dyn Runner + Send>,
    ledger: Arc<deepseeknova_provider::cost::CostLedger>,
    prices: deepseeknova_provider::cost::PriceTable,
    dir: std::path::PathBuf,
    session_id: String,
}

impl MetricsRunner {
    fn new(
        inner: Box<dyn Runner + Send>,
        ledger: Arc<deepseeknova_provider::cost::CostLedger>,
        prices: deepseeknova_provider::cost::PriceTable,
        dir: std::path::PathBuf,
    ) -> Self {
        Self {
            inner,
            ledger,
            prices,
            dir,
            session_id: deepseeknova_metrics::new_session_id(),
        }
    }
}

#[async_trait]
impl Runner for MetricsRunner {
    async fn run_stream(&self, input: RunInput) -> anyhow::Result<RunEventStream> {
        let inner_stream = self.inner.run_stream(input).await?;
        let ledger = Arc::clone(&self.ledger);
        let prices = self.prices.clone();
        let dir = self.dir.clone();
        let session_id = self.session_id.clone();
        let mut tracker = deepseeknova_metrics::SessionTracker::new();
        let stream = inner_stream.map(move |ev| {
            match &ev {
                // Coordinator 的工具调用在 GraphExecutor 内部完成，事件流里
                // 只有收尾的 ToolResult（call_id=节点 id）；按节点计步骤。
                Ok(RunEvent::ToolResult { .. }) => {
                    tracker.observe_step();
                    tracker.observe_tool_call("coordinator", true);
                }
                // 非工具节点（think/reflect）以 [node_id] 文本事件呈现。
                Ok(RunEvent::TextDelta(text)) if !text.starts_with("[PLAN]") => {
                    tracker.observe_step();
                }
                Ok(RunEvent::Done(_)) => {
                    tracker.mark_outcome(deepseeknova_metrics::RunOutcome::Completed);
                    let report = deepseeknova_metrics::SessionReport {
                        session_id: session_id.clone(),
                        stats: tracker.snapshot(),
                        cost: ledger.report(&prices),
                    };
                    if let Err(e) = deepseeknova_metrics::write_report(&report, &dir) {
                        tracing::warn!("coordinator metrics write failed: {e}");
                    }
                }
                _ => {}
            }
            ev
        });
        Ok(Box::pin(stream))
    }
}

/// 解析并校验扫描根：词法归一 + 规范化双重包含性检查。
/// `..`/绝对路径逃逸与 symlink 逃逸均 fail-closed 中止；仅"路径不存在"
/// 这类非安全失败回落归一化路径（扫描结果为空，不泄露）。
fn resolve_scan_root(
    workspace: &std::path::Path,
    raw: &std::path::Path,
) -> anyhow::Result<std::path::PathBuf> {
    let abs = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        workspace.join(raw)
    };
    let norm = normalize_path(&abs);
    if !norm.starts_with(workspace) {
        anyhow::bail!("scan path escapes the workspace root: {}", raw.display());
    }
    // symlink 逃逸：canonicalize 解析符号链接后复核包含性。
    if let Ok(canon) = std::fs::canonicalize(&norm) {
        let ws_canon = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
        if !canon.starts_with(&ws_canon) {
            anyhow::bail!(
                "scan path escapes the workspace root via symlink: {}",
                raw.display()
            );
        }
    }
    Ok(norm)
}

/// 词法归一化路径：折叠 `..`、丢弃 `.`（保留根前缀）。
/// 与 security crate 的 normalize_path 语义一致，用于 resolve_scan_root 的
/// starts_with 预检查（词法前缀匹配不折叠 `..`，必须先归一）。
fn normalize_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut normalized = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            _ => {
                normalized.push(component);
            }
        }
    }
    normalized
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        // P0 Fix: Find the largest UTF-8 character boundary at or before max.
        // `&s[..max]` panics if max splits a multi-byte character.
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_at_files_skips_noise_dirs_and_caps() {
        let ws = tempfile::tempdir().unwrap();
        let root = ws.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join(".deepseeknova")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main(){}").unwrap();
        std::fs::write(root.join("src/lib.rs"), "fn f(){}").unwrap();
        std::fs::write(root.join("target/x.txt"), "noise").unwrap();
        std::fs::write(root.join(".git/config"), "noise").unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]").unwrap();

        let old = std::env::current_dir().unwrap();
        std::env::set_current_dir(root).unwrap();
        let files = collect_at_files();
        std::env::set_current_dir(&old).unwrap();

        assert!(files.contains(&"Cargo.toml".to_string()));
        assert!(files.contains(&"src/main.rs".to_string()));
        assert!(files.contains(&"src/lib.rs".to_string()));
        assert!(
            !files.iter().any(|f| f.contains("target/")
                || f.contains(".git/")
                || f.contains(".deepseeknova/")),
            "噪声目录必须被跳过: {files:?}"
        );
    }

    #[test]
    fn collect_at_files_skips_symlink_cycles() {
        // 回归：目录 symlink 指向自身/祖先会形成环，跟随则无限递归挂起
        // 启动。`file_type().is_symlink()` 不跟随链接，必须直接跳过。
        let ws = tempfile::tempdir().unwrap();
        let root = ws.path();
        std::fs::write(root.join("real.txt"), "x").unwrap();
        std::os::unix::fs::symlink(root, root.join("loop")).unwrap();
        std::os::unix::fs::symlink("real.txt", root.join("ln.txt")).unwrap();

        let old = std::env::current_dir().unwrap();
        std::env::set_current_dir(root).unwrap();
        let files = collect_at_files();
        std::env::set_current_dir(&old).unwrap();

        assert!(files.contains(&"real.txt".to_string()));
        assert!(
            !files.iter().any(|f| f.starts_with("loop") || f == "ln.txt"),
            "symlink 一律跳过，不成环也不收录: {files:?}"
        );
    }

    #[test]
    fn sessions_root_honors_session_config() {
        let mut c = deepseeknova_config::Config::default();
        assert!(sessions_root(&c).is_some(), "default = enabled, home path");
        c.session.root = "/tmp/custom-sessions".into();
        assert_eq!(
            sessions_root(&c).unwrap(),
            std::path::PathBuf::from("/tmp/custom-sessions")
        );
        c.session.enabled = false;
        assert!(sessions_root(&c).is_none(), "disabled kills persistence");
    }

    #[test]
    fn review_provider_none_when_disabled() {
        // review.enabled = false → 不构建 review provider（避免无关路径因
        // quick 指针的 API key 缺失而阻断 agent 构建）。
        let config = deepseeknova_config::Config::default();
        let router = deepseeknova_provider::router::ModelRouter::from_config(
            &config,
            std::sync::Arc::new(deepseeknova_provider::cost::CostLedger::new()),
        )
        .unwrap();
        assert!(review_provider_for(&router, &config).unwrap().is_none());
    }

    #[test]
    fn compact_override_prefers_pointer_over_compact_model() {
        // 指针未设 + compact_model 非空 → override 为 compact_model
        let mut c = deepseeknova_config::Config::default();
        c.agent.compact_model = "cheap".into();
        assert_eq!(compact_override_model(&c), Some("cheap"));
        // 指针已设 → 指针胜，无 override
        c.model_pointers.compact = Some("ptr-model".into());
        assert_eq!(compact_override_model(&c), None);
        // 双无 → 无 override
        c.model_pointers.compact = None;
        c.agent.compact_model.clear();
        assert_eq!(compact_override_model(&c), None);
    }

    #[test]
    fn cli_session_label_is_serve_safe() {
        // F11：会话标注必须是 serve 端点 id 白名单可接受的形态
        // （`[A-Za-z0-9_-]`，否则 Paused 透出的 id 无法用于端点）。
        let label = cli_session_label();
        assert!(label.starts_with("session-"), "unexpected label: {label}");
        assert!(
            label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "label must only contain [A-Za-z0-9_-] (serve path whitelist): {label}"
        );
    }

    // ── resolve_scan_root（fail-closed 逃逸检查） ───────────────────────

    fn temp_scan_root(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("dpr-cli-scan-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn scan_root_aborts_on_parent_traversal() {
        let root = temp_scan_root("traversal");
        for bad in ["..", "../..", "../../etc/passwd"] {
            let err = resolve_scan_root(&root, std::path::Path::new(bad)).unwrap_err();
            assert!(
                err.to_string().contains("escapes the workspace root"),
                "`{bad}` must fail-closed, got: {err}"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_root_aborts_on_absolute_escape() {
        let root = temp_scan_root("abs-escape");
        let err = resolve_scan_root(&root, std::path::Path::new("/etc")).unwrap_err();
        assert!(err.to_string().contains("escapes the workspace root"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_root_allows_inner_path() {
        let root = temp_scan_root("inner");
        std::fs::create_dir_all(root.join("a/b/c")).unwrap();
        let res = resolve_scan_root(&root, std::path::Path::new("a/b/c")).unwrap();
        assert_eq!(res, root.join("a/b/c"));
        let _ = std::fs::remove_dir_all(&root);
    }

    // symlink 场景依赖 unix 的 symlink()；非 unix 下链接不存在时
    // canonicalize 失败走回落分支（返回 Ok），因此整个测试 cfg 门控。
    #[cfg(unix)]
    #[test]
    fn scan_root_aborts_on_symlink_escape() {
        let ws = std::env::temp_dir().join(format!("dnv-symlink-{}", std::process::id()));
        let outside = ws.with_extension("outside"); // 同级外部目录
        std::fs::create_dir_all(outside.join("sub")).unwrap();
        std::fs::write(outside.join("sub/secret.rs"), "let api_key = \"sk-x\";\n").unwrap();
        std::fs::create_dir_all(&ws).unwrap();
        let _ = std::fs::remove_file(ws.join("link"));
        std::os::unix::fs::symlink(&outside, ws.join("link")).unwrap();
        let ws_root = std::path::PathBuf::from(&ws);
        let err = resolve_scan_root(&ws_root, std::path::Path::new("link/sub")).unwrap_err();
        assert!(err.to_string().contains("symlink"));
        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn severity_min_filter_direction() {
        use deepseeknova_scanner::rule::Severity;
        // "medium" 下限应保留 High 与 Medium，排除 Low。
        let min = Severity::Medium;
        assert!(Severity::High <= min, "High kept under medium floor");
        assert!(Severity::Medium <= min);
        assert!(!(Severity::Low <= min), "Low excluded under medium floor");
    }
}
