//! 失败诊断钩子：报告落盘、留存、失败模式聚类/脱敏与回灌。
//! M7b 拆分：从 lib.rs 纯搬移，不修改行为/签名。

use std::path::PathBuf;
use std::sync::Arc;

use deepseeknova_config::Config;
use deepseeknova_security::quality::redact_secrets;

use crate::metrics::{backfill_scorecard_task_rate, enforce_metrics_retention};

/// 诊断报告留存上限：对齐 `[metrics] max_reports` 配置默认值（100）。诊断
/// 子目录与 metrics 目录共用同一留存语义（`enforce_metrics_retention`）。
const DIAGNOSE_RETENTION_MAX: usize = 100;

/// 按目录为 Agent 挂载失败诊断钩子（任务质量闭环 B 阶段）：run 以非
/// success 结束（Paused/failed）时，回调闭包在 `<dir>/diagnose/` 子目录
/// （不存在则建）写 `<session_id>.json`，并复用 [`crate::enforce_metrics_retention`]
/// 对诊断子目录执行留存（上限对齐 `[metrics] max_reports` 默认值 100）。
/// 无条件装配（低风险旁路）：写入/留存失败仅 warn，不阻断 run；成功结束
/// 不产生任何文件。
///
/// 委托 [`attach_diagnose_hook_with_ingest`]（`None` 配置 = 不启用失败模式
/// 聚类），保持既有签名与语义不变。
pub fn attach_diagnose_hook(
    agent: deepseeknova_agent::Agent,
    dir: PathBuf,
) -> deepseeknova_agent::Agent {
    attach_diagnose_hook_with_ingest(agent, dir, None, std::path::Path::new(""))
}

/// [`attach_diagnose_hook`] 的协议增强扩展：`[protocol] enabled=true` 且
/// `workspace_root` 非空时，除原落盘/留存逻辑外，把本会话
/// [`DiagnoseReport`](deepseeknova_agent::diagnose::DiagnoseReport) 的
/// `failures` 逐条聚类进
/// [`FailurePatternStore`](deepseeknova_security::failure_pattern::FailurePatternStore)
/// （`<workspace_root>/.deepseeknova/security/failure-patterns.json`，协议增强
/// 设计 §6）并 save。字段映射：`FailureDetail.phase → phase`、
/// `.tool → tool`、`.error → error`、`.root_cause.or(fix_plan) → lesson`。
/// 注入内容先脱敏（spec §6.2）：error/lesson 过
/// [`redact_secrets`] 再 ingest，防止密钥原文进模式库并被下会话回灌进
/// system prompt（接线侧最后防线；security 侧 ingest 入口另有双保险）。
/// 诊断钩子天然只在非 success 结束时触发，满足「仅失败会话 ingest」语义；
/// 此外无论 `[protocol]` 开关，回调都会对同会话评分卡做 task_rate 回填
/// （设计 §7.1 末条：按 failures 推导 `first_pass`/`retry_rounds` 覆写并
/// 重写 `dir/<session_id>.scorecard.json`，补 Paused 路径上 metrics hook
/// 先触发时缺失的失败信息；**仅 failures 非空时覆写**，零失败报告保持
/// metrics 侧已填值，见 P-L2；评分卡不存在时静默跳过）；
/// 所有 IO 失败仅 warn，不阻断 run。`None` 配置或 disabled 时与
/// [`attach_diagnose_hook`] 完全一致。
pub fn attach_diagnose_hook_with_ingest(
    agent: deepseeknova_agent::Agent,
    dir: PathBuf,
    config: Option<&Config>,
    workspace_root: &std::path::Path,
) -> deepseeknova_agent::Agent {
    let diagnose_dir = dir.join("diagnose");
    // 协议增强：聚类仅在 `[protocol] enabled` 且提供 workspace 时启用。
    let ingest_on = config
        .map(|c| c.protocol.enabled && !workspace_root.as_os_str().is_empty())
        .unwrap_or(false);
    let patterns_path = workspace_root
        .join(".deepseeknova")
        .join("security")
        .join("failure-patterns.json");
    let hook: deepseeknova_agent::diagnose::DiagnoseHook = Arc::new(move |report| {
        // DiagnoseReport::write_to 负责建目录 + 写 `<session_id>.json`；
        // 失败仅 warn（与 attach_metrics_hook 落盘同模式，不阻断 run）。
        if let Err(e) = report.write_to(&diagnose_dir) {
            tracing::warn!("diagnose report write failed: {e}");
            return;
        }
        enforce_metrics_retention(&diagnose_dir, DIAGNOSE_RETENTION_MAX);

        // task_rate 回填（设计 §7.1 末条）：Paused/unverified 路径上 metrics
        // hook 先于本回调触发（失败详情尚不可知，评分卡已按保守默认 false/0
        // 落盘），此处按本会话 failures 覆写并重写同会话评分卡
        // （`<dir>/<session_id>.scorecard.json`）。零失败（Cancelled/unverified
        // 无工具失败详情）**不覆写**，保持 metrics hook 已填的值，避免零失败
        // 会话被误标 first_pass=true（P-L2）。评分卡缺失/不可解析时静默跳过
        // （metrics 未启用），不 panic、不阻断。
        backfill_scorecard_task_rate(&dir, &report);

        // 协议增强：失败模式聚类（仅 protocol.enabled 时动作）。
        if ingest_on {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            match deepseeknova_security::failure_pattern::FailurePatternStore::load(&patterns_path)
            {
                Ok(mut store) => {
                    ingest_failure_patterns(&mut store, &report.failures, now_ms);
                    if let Err(e) = store.save() {
                        tracing::warn!("failure pattern store save failed: {e}");
                    }
                }
                Err(e) => tracing::warn!("failure pattern store load failed: {e}"),
            }
        }
    });
    agent.with_diagnose_hook(hook)
}

/// 失败模式聚类写入（协议增强 §6）：把 `failures` 逐条 ingest 进 store。
/// 注入内容脱敏（spec §6.2）：每条 `FailureDetail` 的 `error` 与 `lesson`
/// （`root_cause.or(fix_plan)`）先过 [`redact_secrets`] 再 ingest，防止密钥/
/// 凭据原文进 failure-patterns.json 并被下会话回灌进 system prompt（接线侧
/// 最后防线；security 侧 ingest 入口另有双保险）。
fn ingest_failure_patterns(
    store: &mut deepseeknova_security::failure_pattern::FailurePatternStore,
    failures: &[deepseeknova_agent::diagnose::FailureDetail],
    now_ms: u64,
) {
    for f in failures {
        let lesson = f
            .root_cause
            .as_deref()
            .or(f.fix_plan.as_deref())
            .map(str::to_string);
        let error = redact_secrets(&f.error);
        let lesson = lesson.as_deref().map(redact_secrets);
        store.ingest(
            &f.phase,
            f.tool.as_deref(),
            &error,
            lesson.as_deref(),
            now_ms,
        );
    }
}

/// 失败模式回灌（协议增强设计 §6.2）：`[protocol] enabled=true` 时，会话
/// 启动前（run 开始前）从 `<workspace_root>/.deepseeknova/security/
/// failure-patterns.json` 加载历史失败模式库，`suggest(3)` 取 top-3 后追加
/// `## 本会话已知失败模式（自动注入）` 块到首轮 system prompt（复用
/// `Agent::with_appended_system_prompt` 先例，见 graph 检索提示注入）。
///
/// 无模式 / store 缺失 / IO 失败时零注入（仅 warn），`enabled=false` 时
/// 原样返回，Agent 行为零变化。本函数只做回灌，不涉及门控/对抗审查
/// （见 [`crate::attach_protocol_gates`]）。
pub fn attach_failure_pattern_injection(
    agent: deepseeknova_agent::Agent,
    config: &Config,
    workspace_root: &std::path::Path,
) -> deepseeknova_agent::Agent {
    if !config.protocol.enabled {
        return agent;
    }
    let path = workspace_root
        .join(".deepseeknova")
        .join("security")
        .join("failure-patterns.json");
    let suggestions = match deepseeknova_security::failure_pattern::FailurePatternStore::load(&path)
    {
        Ok(store) => store.suggest(3),
        Err(e) => {
            tracing::warn!("failure pattern store load failed: {e}");
            Vec::new()
        }
    };
    if suggestions.is_empty() {
        return agent;
    }
    let mut block = String::from("\n\n## 本会话已知失败模式（自动注入）\n");
    for s in &suggestions {
        block.push_str(&format!("- {s}\n"));
    }
    agent.with_appended_system_prompt(block)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    /// 任务质量闭环 B：attach_diagnose_hook 仅失败 run 在 `<dir>/diagnose/`
    /// 落盘 `<session_id>.json`；成功 run 不产生新文件。
    #[tokio::test]
    async fn diagnose_hook_writes_report_for_failed_run_only() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;

        let dir = std::env::temp_dir().join(format!("dsn-diag-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        // 失败 run：空内容 → MaxSteps → Paused → 报告落盘。
        let agent = attach_diagnose_hook(
            deepseeknova_agent::Agent::new(Arc::new(EmptyProvider), 2),
            dir.clone(),
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
        let diag_dir = dir.join("diagnose");
        let mut files: Vec<String> = std::fs::read_dir(&diag_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(files.len(), 1, "expected exactly one diagnose report");
        let report: deepseeknova_agent::diagnose::DiagnoseReport =
            serde_json::from_str(&std::fs::read_to_string(diag_dir.join(&files[0])).unwrap())
                .unwrap();
        assert_eq!(report.outcome, "paused");
        assert!(!report.phases.is_empty(), "phases must be recorded");
        assert!(!report.failures.is_empty(), "failures must be non-empty");

        // 成功 run：不新增报告文件。
        let agent = attach_diagnose_hook(
            deepseeknova_agent::Agent::new(Arc::new(stub_provider()), 3),
            dir.clone(),
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
        files = std::fs::read_dir(&diag_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(files.len(), 1, "success must not add a diagnose report");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // 协议增强能力包（阶段4）：失败模式回灌 / ingest 聚类 / fitness 记录
    // -----------------------------------------------------------------------

    /// 协议增强 §6.2：`[protocol] enabled=true` 且 store 有模式时，回灌注入
    /// 首轮 system prompt；≤3 条；enabled=false 时零注入。
    #[tokio::test]
    async fn failure_pattern_injection_injects_up_to_three() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;

        let root = std::env::temp_dir().join(format!("dsn-protocol-inj-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".deepseeknova/security")).unwrap();

        // 构造 store：4 条模式（count 4/3/2/1）。
        let mut store = deepseeknova_security::failure_pattern::FailurePatternStore::load(
            &root.join(".deepseeknova/security/failure-patterns.json"),
        )
        .unwrap();
        for (i, err) in ["err-a", "err-b", "err-c", "err-d"].iter().enumerate() {
            for _ in 0..(4 - i) {
                store.ingest("execute", Some("bash"), err, None, 1000 + i as u64);
            }
        }
        store.save().unwrap();

        // enabled=true：注入（捕获首轮 system prompt）。
        struct PromptCapture {
            system: Arc<std::sync::Mutex<Option<String>>>,
        }
        #[async_trait::async_trait]
        impl deepseeknova_provider::Provider for PromptCapture {
            async fn generate(
                &self,
                validated: deepseeknova_provider::ValidatedRequest<'_>,
            ) -> Result<deepseeknova_core::Message, deepseeknova_core::DeepseeknovaError>
            {
                *self.system.lock().unwrap() = validated
                    .messages
                    .iter()
                    .find(|m| m.role == deepseeknova_core::Role::System)
                    .map(|m| m.content.clone());
                Ok(deepseeknova_core::Message {
                    role: deepseeknova_core::Role::Assistant,
                    content: "ok".to_string(),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                    reasoning_signature: None,
                    usage: None,
                })
            }
        }
        let mut config = Config::default();
        config.protocol.enabled = true;
        config.graph.enabled = false;
        config.memory.enabled = false;
        config.delegate.enabled = false;
        let capture = Arc::new(std::sync::Mutex::new(None));
        let provider: Arc<dyn deepseeknova_provider::Provider> = Arc::new(PromptCapture {
            system: capture.clone(),
        });
        let agent = attach_failure_pattern_injection(
            deepseeknova_agent::Agent::new(provider, 2),
            &config,
            &root,
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
        let system = capture.lock().unwrap().clone().expect("system prompt set");
        assert!(system.contains("## 本会话已知失败模式（自动注入）"));
        // 3 条模式（top-3 by count），第 4 条不注入。
        assert_eq!(system.matches("- [失败模式]").count(), 3);
        assert!(system.contains("err-a") && system.contains("err-b") && system.contains("err-c"));
        assert!(
            !system.contains("err-d"),
            "4th pattern must not be injected"
        );

        // enabled=false：零注入。
        let mut config_off = Config::default();
        config_off.protocol.enabled = false;
        config_off.graph.enabled = false;
        config_off.memory.enabled = false;
        config_off.delegate.enabled = false;
        let capture_off = Arc::new(std::sync::Mutex::new(None));
        let provider_off: Arc<dyn deepseeknova_provider::Provider> = Arc::new(PromptCapture {
            system: capture_off.clone(),
        });
        let agent = attach_failure_pattern_injection(
            deepseeknova_agent::Agent::new(provider_off, 2),
            &config_off,
            &root,
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
        let system_off = capture_off.lock().unwrap().clone().unwrap_or_default();
        assert!(
            !system_off.contains("本会话已知失败模式"),
            "protocol disabled must not inject"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 协议增强 §6.1：失败 run 结束后，diagnose hook 把 failures 聚类进
    /// failure-patterns.json（phase/tool/error/lesson 映射）；成功 run 不产生
    /// 模式文件（无 diagnose 报告）。enabled=false 时不写模式文件。
    #[tokio::test]
    async fn failure_pattern_ingest_clusters_from_diagnose() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;

        let root = std::env::temp_dir().join(format!("dsn-protocol-ingest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let metrics_dir = root.join(".deepseeknova/metrics");
        let patterns_path = root.join(".deepseeknova/security/failure-patterns.json");

        let mut config = Config::default();
        config.protocol.enabled = true;

        // 失败 run（EmptyProvider → MaxSteps → Paused → diagnose 报告）。
        let agent = attach_diagnose_hook_with_ingest(
            deepseeknova_agent::Agent::new(Arc::new(EmptyProvider), 2),
            metrics_dir.clone(),
            Some(&config),
            &root,
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
        // 模式文件已生成且非空（failures 非空 → 至少 1 簇）。
        let store =
            deepseeknova_security::failure_pattern::FailurePatternStore::load(&patterns_path)
                .unwrap();
        assert!(
            !store.suggest(3).is_empty(),
            "failed run must cluster at least one pattern"
        );

        // enabled=false：不写模式文件。
        let root2 =
            std::env::temp_dir().join(format!("dsn-protocol-ingest-off-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root2);
        std::fs::create_dir_all(&root2).unwrap();
        let metrics_dir2 = root2.join(".deepseeknova/metrics");
        let patterns_path2 = root2.join(".deepseeknova/security/failure-patterns.json");
        let mut config_off = Config::default();
        config_off.protocol.enabled = false;
        let agent = attach_diagnose_hook_with_ingest(
            deepseeknova_agent::Agent::new(Arc::new(EmptyProvider), 2),
            metrics_dir2.clone(),
            Some(&config_off),
            &root2,
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
        assert!(
            !patterns_path2.exists(),
            "protocol disabled must not create failure-patterns.json"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&root2);
    }

    /// 协议增强 §6.2：聚类入口 ingest 前对 failures 的 error/lesson 过
    /// `redact_secrets`——构造含密钥原文（AWS AKIA 键 + PEM 私钥头）的
    /// failures 直连 [`ingest_failure_patterns`]，断言落盘文件不含密钥原文、
    /// 只含 `[REDACTED]` 标记（接线侧最后防线；security 侧 ingest 入口另有
    /// 双保险）。
    #[test]
    fn failure_pattern_ingest_redacts_secrets_before_write() {
        use deepseeknova_agent::diagnose::FailureDetail;

        let root =
            std::env::temp_dir().join(format!("dsn-protocol-ingest-redact-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let patterns_path = root.join(".deepseeknova/security/failure-patterns.json");

        let aws_key = "AKIAIOSFODNN7EXAMPLE";
        let pem = "-----BEGIN RSA PRIVATE KEY-----";
        let failures = vec![
            FailureDetail {
                phase: "tool".into(),
                tool: Some("bash".into()),
                command: Some("aws s3 ls".into()),
                error: format!("Error: credentials {aws_key} rejected"),
                root_cause: Some(format!("env dump leaked {aws_key}")),
                fix_plan: None,
            },
            FailureDetail {
                phase: "plan".into(),
                tool: None,
                command: None,
                error: format!("config load failed: {pem}\nMIIEpAIBAAK..."),
                root_cause: None,
                fix_plan: Some(format!("rotate key material behind {pem}")),
            },
        ];
        let mut store =
            deepseeknova_security::failure_pattern::FailurePatternStore::load(&patterns_path)
                .unwrap();
        ingest_failure_patterns(&mut store, &failures, 1);
        store.save().unwrap();

        let text = std::fs::read_to_string(&patterns_path).unwrap();
        assert!(
            !text.contains(aws_key),
            "raw AWS key must not be persisted into failure-patterns.json"
        );
        assert!(
            !text.contains(pem),
            "raw PEM private key header must not be persisted"
        );
        assert!(
            text.contains("[REDACTED]"),
            "redacted marker must be persisted for secret-bearing failures"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
