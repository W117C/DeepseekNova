//! 会话效能：MetricsSink 落盘、留存策略、metrics/fitness 钩子、task_rate 回填。
//! M7b 拆分：从 lib.rs 纯搬移，不修改行为/签名。

use std::path::PathBuf;
use std::sync::Arc;

use deepseeknova_config::Config;

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
/// （CLI）经 [`crate::build_agent_with_role_providers`] 的注入侧收集器回填真实注入
/// 的技能名（spec §13 #9 接线完成）；集合为空 = 本会话无注入技能，优雅跳过
/// （不写文件、不 warn）。`enabled=false` 或空 workspace 时
/// 行为与 [`attach_metrics_hook`] 完全一致。
///
/// task_rate（设计 §7.1 末条）：Completed 结束在评分卡落盘前按
/// `first_pass=true` 填写；Paused/Cancelled 路径本 hook 先于诊断回调触发、
/// 失败详情尚不可知，维持保守默认（false/0），由
/// [`crate::attach_diagnose_hook_with_ingest`] 的诊断回调按 failures 覆写。
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

/// task_rate 回填决策（设计 §7.1 末条 + P-L2）：仅当诊断报告 `failures`
/// **非空**（失败型会话）时覆写评分卡 `first_pass=false` 与
/// `retry_rounds=失败条数`；零失败报告（如 Cancelled/unverified 无工具失败
/// 详情）**不覆写**，保持 metrics hook 已填的值（非 Completed 的保守
/// false/0），避免零失败会话被误标 first_pass=true。评分卡缺失/不可解析时
/// 静默跳过（metrics 未启用属正常路径）；IO 失败仅 warn，不阻断 run。
pub(crate) fn backfill_scorecard_task_rate(
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
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

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

    /// 留存防御：max_reports=0 视为不清理（即使目录已有报告）。
    #[test]
    fn metrics_retention_max_reports_zero_is_noop() {
        let dir = std::env::temp_dir().join(format!("dsn-metrics-zero-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for n in ["a.json", "b.json", "c.json"] {
            std::fs::write(dir.join(n), "{}").unwrap();
        }
        enforce_metrics_retention(&dir, 0);
        let count = std::fs::read_dir(&dir).unwrap().count();
        assert_eq!(count, 3, "max_reports=0 不清理");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 留存容错：目录不存在时静默跳过（不 panic）。
    #[test]
    fn metrics_retention_missing_dir_is_silent() {
        let dir = std::env::temp_dir().join(format!("dsn-metrics-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); // 确保不存在
        enforce_metrics_retention(&dir, 5);
    }

    /// F12 大小写双向：大写 `.JSON` 报告同样参与留存裁剪（按 mtime 删最旧），
    /// 而非只识别小写扩展名。
    #[test]
    fn metrics_retention_counts_uppercase_json_reports() {
        let dir = std::env::temp_dir().join(format!("dsn-metrics-upjson-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // z.JSON 最旧（大写），a.json 最新。
        std::fs::write(dir.join("z.JSON"), "{}").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(25));
        std::fs::write(dir.join("a.json"), "{}").unwrap();
        enforce_metrics_retention(&dir, 1);
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["a.json".to_string()],
            "大写 .JSON 应参与裁剪（z 被删）"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
