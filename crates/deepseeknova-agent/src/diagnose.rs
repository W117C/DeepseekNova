//! 结构化失败诊断报告（任务质量闭环 B 阶段）。
//!
//! 失败/Paused 结束路径聚合「阶段分解 + 时序 + 失败详情 + 子代理
//! drill-down + 本会话质量 findings」为单份机读 JSON。agent 主循环在关键点
//! 记录最小时间戳集与失败详情，终端路径经 [`DiagnoseGuard`] 构造
//! [`DiagnoseReport`] 传给诊断回调（runtime 装配落盘）；构造或回调失败
//! 不影响主流程。

use crate::reflection::Reflection;
use deepseeknova_core::tool_hook::QualityFinding;
use deepseeknova_core::{Message, Role};
use deepseeknova_security::quality::redact_secrets;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

/// 诊断回调（runtime 注入落盘；None = 关闭）。
pub type DiagnoseHook = Arc<dyn Fn(DiagnoseReport) + Send + Sync>;

/// 一个执行阶段的时间跨度。
///
/// `started_at_ms` / `ended_at_ms` 为**相对 run 起始的偏移毫秒**（`Instant`
/// 单调时钟，避免系统时间回拨导致时序倒挂）；`duration_ms = ended - started`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseSpan {
    /// 相位名：`plan` / `tool` / `verify` / `reflect`。
    pub name: String,
    /// 相对 run 起始的偏移毫秒。
    pub started_at_ms: u64,
    /// 相对 run 起始的偏移毫秒。
    pub ended_at_ms: u64,
    /// 相位持续毫秒。
    pub duration_ms: u64,
}

/// 一个失败点的详情。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureDetail {
    /// 失败归属阶段（`verify` / `review` / `budget` / `tool` / `plan`）。
    pub phase: String,
    /// 涉及的工具名（如 `bash`）；非工具失败为 `None`。
    pub tool: Option<String>,
    /// 涉及的命令（bash 工具参数中的 `command`）；不可得为 `None`。
    pub command: Option<String>,
    /// 错误摘要。
    pub error: String,
    /// 根因（最近一次反思产物；未反思为 `None`）。
    pub root_cause: Option<String>,
    /// 修复计划（最近一次反思产物；未反思为 `None`）。
    pub fix_plan: Option<String>,
}

/// 一个子代理执行跨度。
///
/// 数据源为近似：agent 侧无子代理执行记录（delegate 以普通工具调用执行），
/// preset 从 delegate 调用参数 `agent` 字段提取，outcome 由结果文本启发式
/// 判定，`duration_ms` 无法从历史恢复 → 0。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubAgentSpan {
    /// 子代理预设名（delegate 参数 `agent`）。
    pub preset: String,
    /// 结果：`success` / `failed` / `aborted`。
    pub outcome: String,
    /// 持续毫秒（近似：历史不可得时恒为 0）。
    pub duration_ms: u64,
}

/// 结构化失败诊断报告。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnoseReport {
    /// 会话 id（session_label；未标注时为 run 内生成的兜底 id）。
    pub session_id: String,
    /// 结束态：`success` / `paused` / `failed` / `unverified`（协议启用 +
    /// verify 已配置 + 会话 Complete 但 verify-evidence 硬门未通过时的证据
    /// 链判定结果，spec §4.1；此时仍产报告而非 suppress）。
    pub outcome: String,
    /// 阶段时间跨度（相对 run 起始偏移毫秒）。
    pub phases: Vec<PhaseSpan>,
    /// 失败详情（工具失败 + verify/review/budget/max-steps 终端失败）。
    pub failures: Vec<FailureDetail>,
    /// 子代理 drill-down（近似，见 [`SubAgentSpan`]）。
    pub sub_agents: Vec<SubAgentSpan>,
    /// 本会话累计的质量 findings。
    pub quality: Vec<QualityFinding>,
    /// 对抗审查产出文本（会话收尾触发 adversarial-review 子代理；未触发/
    /// 跳过时为 `None`）。向后兼容：旧报告反序列化时缺省为 `None`。
    #[serde(default)]
    pub adversarial_review: Option<String>,
    /// 报告生成时间（unix 毫秒）。
    pub generated_at_ms: u64,
}

impl DiagnoseReport {
    /// 构造空报告（默认构造辅助；字段留空由调用方填充）。
    pub fn new(session_id: impl Into<String>, outcome: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            outcome: outcome.into(),
            phases: Vec::new(),
            failures: Vec::new(),
            sub_agents: Vec::new(),
            quality: Vec::new(),
            adversarial_review: None,
            generated_at_ms: now_millis(),
        }
    }

    /// 将报告写为 `<dir>/<session_id>.json`（目录不存在则创建）。供 runtime
    /// 诊断回调落盘使用；失败只由调用方 warn，不阻断 run。
    ///
    /// F6 安全加固：落盘前对 `failures[].error/command/tool` 与
    /// `quality[].evidence` 过密钥脱敏（`[REDACTED]` 替换，复用 security 的
    /// [`redact_secrets`]）；Unix 下以 0600 权限写盘（报告可能含命令与错误
    /// 详情，对同机其他用户不可读；非 Unix 平台退化默认行为）。
    pub fn write_to(&self, dir: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!("{}.json", self.session_id));
        let bytes = serde_json::to_vec_pretty(&self.redacted())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true).mode(0o600);
            let mut f = opts.open(&path)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
            // 显式断言权限（OpenOptionsExt::mode 在已存在文件上可能被 umask
            // 影响部分位；set_permissions 强制收敛到 0600）。
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&path, bytes)?;
        }
        Ok(path)
    }

    /// 脱敏副本：`failures[].error/command/tool` 与 `quality[].evidence` 中
    /// 的密钥串替换为 `[REDACTED]`（复用 security 内置正则模式）。
    fn redacted(&self) -> DiagnoseReport {
        let mut r = self.clone();
        for f in &mut r.failures {
            f.error = redact_secrets(&f.error);
            if let Some(c) = &f.command {
                f.command = Some(redact_secrets(c));
            }
            if let Some(t) = &f.tool {
                f.tool = Some(redact_secrets(t));
            }
        }
        for q in &mut r.quality {
            q.evidence = redact_secrets(&q.evidence);
        }
        // 对抗审查文本可能引用工具参数/证据，同样过脱敏。
        r.adversarial_review = self.adversarial_review.as_ref().map(|s| redact_secrets(s));
        r
    }
}

/// run 内诊断数据收集器（同步、非共享）。每个 run 实例化一个，失败/Paused
/// 终端路径快照为 [`DiagnoseReport`] 经 [`DiagnoseGuard`] 传出。
pub struct DiagnoseCollector {
    run_started: std::time::Instant,
    /// 已闭合的相位跨度。
    spans: Vec<PhaseSpan>,
    /// 当前打开的相位 (name, started_offset_ms)。
    current: Option<(String, u64)>,
    /// 终端失败（verify/review/budget/max-steps；工具失败由内存扫描补充）。
    failures: Vec<FailureDetail>,
    /// 最近一次反思产物（失败详情取 root_cause/fix_plan 用，best-effort）。
    last_reflection: Option<Reflection>,
    /// 子代理跨度（emit 时从历史近似提取）。
    sub_agents: Vec<SubAgentSpan>,
}

impl Default for DiagnoseCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl DiagnoseCollector {
    /// 新建收集器（以当前时刻为 run 起始零刻）。
    pub fn new() -> Self {
        Self {
            run_started: std::time::Instant::now(),
            spans: Vec::new(),
            current: None,
            failures: Vec::new(),
            last_reflection: None,
            sub_agents: Vec::new(),
        }
    }

    /// 相对 run 起始的偏移毫秒（`Instant` 单调时钟）。
    fn now_ms(&self) -> u64 {
        self.run_started.elapsed().as_millis() as u64
    }

    /// 进入相位（同名重复进入幂等；切换时闭合上一相位）。
    pub fn phase_enter(&mut self, name: &'static str) {
        if let Some((cur, _)) = &self.current {
            if cur == name {
                return;
            }
            self.close_current();
        }
        self.current = Some((name.to_string(), self.now_ms()));
    }

    /// 闭合当前相位（无打开相位时为空操作）。
    fn close_current(&mut self) {
        if let Some((name, started)) = self.current.take() {
            let ended = self.now_ms();
            self.spans.push(PhaseSpan {
                name,
                started_at_ms: started,
                ended_at_ms: ended,
                duration_ms: ended.saturating_sub(started),
            });
        }
    }

    /// 记录一次终端失败（工具失败由内存扫描在 emit 时补充）。root_cause /
    /// fix_plan 从最近一次反思产物填充（若有）。
    pub fn record_failure(
        &mut self,
        phase: &str,
        tool: Option<String>,
        command: Option<String>,
        error: String,
    ) {
        let (root_cause, fix_plan) = match &self.last_reflection {
            Some(r) => (Some(r.root_cause.clone()), Some(r.fix_plan.clone())),
            None => (None, None),
        };
        self.failures.push(FailureDetail {
            phase: phase.to_string(),
            tool,
            command,
            error,
            root_cause,
            fix_plan,
        });
    }

    /// 记录最近一次反思产物（失败详情取 root_cause/fix_plan 用）。
    pub fn record_reflection(&mut self, r: Reflection) {
        self.last_reflection = Some(r);
    }

    /// 当前相位名（失败详情归属用；无打开相位时回落 `plan`）。
    pub fn failure_phase(&self) -> String {
        self.current
            .as_ref()
            .map(|(n, _)| n.clone())
            .unwrap_or_else(|| "plan".to_string())
    }

    /// F7：Drop 兜底失败详情的相位推导（不新增字段，用已有 spans/current
    /// 状态）：当前打开相位是 `reflect` / `review` / `tool` 时如实填写；
    /// 否则若已进入过 tool 相位（spans 含 `tool`）→ `tool`；其余（尚未
    /// 执行工具即异常终止）→ `plan`。
    pub fn fallback_failure_phase(&self) -> String {
        if let Some((name, _)) = &self.current {
            if name == "reflect" || name == "review" || name == "tool" {
                return name.clone();
            }
        }
        if self.spans.iter().any(|s| s.name == "tool") {
            return "tool".to_string();
        }
        "plan".to_string()
    }

    /// 快照为诊断报告：闭合当前相位，移出收集的数据。`quality` 为本会话
    /// 累计的 findings（由守卫在 emit 时从会话锁读取）；`adversarial_review`
    /// 为对抗审查产出（未触发时为 `None`）。
    pub fn build_report(
        &mut self,
        session_id: String,
        outcome: String,
        quality: Vec<QualityFinding>,
        adversarial_review: Option<String>,
    ) -> DiagnoseReport {
        self.close_current();
        DiagnoseReport {
            session_id,
            outcome,
            phases: std::mem::take(&mut self.spans),
            failures: std::mem::take(&mut self.failures),
            sub_agents: std::mem::take(&mut self.sub_agents),
            quality,
            adversarial_review,
            generated_at_ms: now_millis(),
        }
    }
}

/// 诊断采集守卫：持有本 run 的 [`DiagnoseCollector`]，保证失败/Paused 结束
/// 路径恰好构造一次报告——显式终端路径先 `emit(outcome)`，`?` 提前返回时由
/// Drop 兜底（outcome=failed，无内存扫描，quality 用 try_lock 尽力读取）。
/// 成功路径调用 `suppress()` 关闭。构造/回调失败不影响主流程。
pub struct DiagnoseGuard {
    collector: DiagnoseCollector,
    hook: Option<DiagnoseHook>,
    session_id: Option<String>,
    quality_findings: Arc<tokio::sync::Mutex<Vec<QualityFinding>>>,
    /// 对抗审查产出（会话收尾注入；emit 时写入报告）。
    adversarial_review: Option<String>,
    emitted: bool,
}

impl DiagnoseGuard {
    /// 新建守卫。`session_id` 为会话标注（Paused 事件同源）；`quality_findings`
    /// 为会话级 findings 累计锁（任务质量闭环 A 阶段）。
    pub fn new(
        hook: Option<DiagnoseHook>,
        session_id: Option<String>,
        quality_findings: Arc<tokio::sync::Mutex<Vec<QualityFinding>>>,
    ) -> Self {
        Self {
            collector: DiagnoseCollector::new(),
            hook,
            session_id,
            quality_findings,
            adversarial_review: None,
            emitted: false,
        }
    }

    /// 注入对抗审查产出（会话收尾触发子代理后调用；`None` 表示未触发/跳过）。
    pub fn set_adversarial_review(&mut self, text: Option<String>) {
        self.adversarial_review = text;
    }

    /// 进入相位（透传收集器）。
    pub fn phase_enter(&mut self, name: &'static str) {
        self.collector.phase_enter(name);
    }

    /// 记录终端失败（透传收集器）。
    pub fn record_failure(
        &mut self,
        phase: &str,
        tool: Option<String>,
        command: Option<String>,
        error: String,
    ) {
        self.collector.record_failure(phase, tool, command, error);
    }

    /// 记录最近反思产物（透传收集器）。
    pub fn record_reflection(&mut self, r: Reflection) {
        self.collector.record_reflection(r);
    }

    /// 当前相位名（透传收集器）。
    pub fn failure_phase(&self) -> String {
        self.collector.failure_phase()
    }

    /// 构造报告并调用诊断回调（若注册）。仅一次；回调 panic 捕获后忽略。
    /// `messages` 为 run 的对话历史（用于提取工具失败与子代理近似）。
    pub async fn emit(&mut self, outcome: &str, messages: &[Message]) {
        if self.emitted {
            return;
        }
        self.emitted = true;
        let Some(hook) = self.hook.clone() else {
            return;
        };
        let quality = self.quality_findings.lock().await.clone();
        let mut failures = collect_tool_failures(messages);
        failures.extend(std::mem::take(&mut self.collector.failures));
        self.collector.failures = failures;
        self.collector.sub_agents = collect_sub_agents(messages);
        let session_id = self
            .session_id
            .clone()
            .unwrap_or_else(|| format!("diag-{}", uuid::Uuid::new_v4()));
        let report = self.collector.build_report(
            session_id,
            outcome.to_string(),
            quality,
            self.adversarial_review.clone(),
        );
        if catch_unwind(AssertUnwindSafe(|| hook(report))).is_err() {
            tracing::warn!("diagnose hook panicked; report delivery skipped");
        }
    }

    /// 标记为已处理（成功结束路径调用，禁止 Drop 兜底产出报告）。
    pub fn suppress(&mut self) {
        self.emitted = true;
    }
}

impl Drop for DiagnoseGuard {
    fn drop(&mut self) {
        if self.emitted {
            return;
        }
        self.emitted = true;
        // 同步 Drop 无法 await：try_lock 失败则跳过 quality 快照。
        let quality = self
            .quality_findings
            .try_lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        // F7：兜底失败相位不再硬编码 `plan`——用收集器状态推导（已进入过
        // tool 相位 → `tool`；当前为 reflect/review 则如实填写）。
        let phase = self.collector.fallback_failure_phase();
        self.collector.record_failure(
            &phase,
            None,
            None,
            "run terminated abnormally (error path)".to_string(),
        );
        let session_id = self
            .session_id
            .clone()
            .unwrap_or_else(|| format!("diag-{}", uuid::Uuid::new_v4()));
        let report = self.collector.build_report(
            session_id,
            "failed".to_string(),
            quality,
            self.adversarial_review.clone(),
        );
        if let Some(hook) = &self.hook {
            let _ = catch_unwind(AssertUnwindSafe(|| hook(report)));
        }
    }
}

/// 截断到 `max` 字节（字符边界），超长追加省略标记。
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let end = s.floor_char_boundary(max);
    format!("{}... [truncated]", &s[..end])
}

/// 从工具参数 JSON 提取可展示的命令串（目前仅 bash 的 `command` 字段）。
fn extract_command(tool: &str, args: &str) -> Option<String> {
    if tool != "bash" {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(args).ok()?;
    v.get("command")
        .and_then(|c| c.as_str())
        .map(|s| s.chars().take(200).collect())
}

/// 从 run 的对话历史提取工具失败详情（phase=`tool`）。错误形态复用 agent
/// 主循环的 `is_tool_error_result` 判定；命令从 bash 参数提取。
fn collect_tool_failures(messages: &[Message]) -> Vec<FailureDetail> {
    // 助手消息的 tool_calls：call_id → (工具名, 参数)。
    let mut calls: HashMap<String, (String, String)> = HashMap::new();
    for m in messages {
        if let Some(tcs) = &m.tool_calls {
            for tc in tcs {
                calls.insert(
                    tc.id.clone(),
                    (tc.function.name.clone(), tc.function.arguments.clone()),
                );
            }
        }
    }
    let mut out = Vec::new();
    for m in messages {
        if m.role != Role::Tool {
            continue;
        }
        let Some(call_id) = &m.tool_call_id else {
            continue;
        };
        let Some((name, args)) = calls.get(call_id) else {
            continue;
        };
        if !crate::agent::is_tool_error_result(&m.content) {
            continue;
        }
        out.push(FailureDetail {
            phase: "tool".to_string(),
            tool: Some(name.clone()),
            command: extract_command(name, args),
            error: truncate(&m.content, 500),
            root_cause: None,
            fix_plan: None,
        });
    }
    out
}

/// 从对话历史识别 delegate 工具调用，近似填充子代理跨度。
///
/// 近似性说明：agent 侧无子代理执行记录（delegate 以普通工具调用执行），
/// preset 从参数 `agent` 字段提取，outcome 由结果文本启发式判定（含
/// `cancelled` → aborted；含 `failed` 或错误形态 → failed；否则 success），
/// `duration_ms` 无法从历史恢复 → 0。
fn collect_sub_agents(messages: &[Message]) -> Vec<SubAgentSpan> {
    let mut calls: HashMap<String, String> = HashMap::new();
    for m in messages {
        if let Some(tcs) = &m.tool_calls {
            for tc in tcs {
                if tc.function.name == "delegate" {
                    calls.insert(tc.id.clone(), tc.function.arguments.clone());
                }
            }
        }
    }
    let mut out = Vec::new();
    for m in messages {
        if m.role != Role::Tool {
            continue;
        }
        let Some(call_id) = &m.tool_call_id else {
            continue;
        };
        let Some(args) = calls.get(call_id) else {
            continue;
        };
        let preset = serde_json::from_str::<serde_json::Value>(args)
            .ok()
            .and_then(|v| v.get("agent").and_then(|a| a.as_str()).map(str::to_owned))
            .unwrap_or_else(|| "unknown".to_string());
        let lower = m.content.to_ascii_lowercase();
        let outcome = if lower.contains("cancelled") {
            "aborted"
        } else if lower.contains("failed") || crate::agent::is_tool_error_result(&m.content) {
            "failed"
        } else {
            "success"
        };
        out.push(SubAgentSpan {
            preset,
            outcome: outcome.to_string(),
            duration_ms: 0,
        });
    }
    out
}

/// 当前 unix 毫秒时间戳（`generated_at_ms` 用）。
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_core::types::{FunctionCall, ToolCall};

    fn tool_msg(id: &str, content: &str) -> Message {
        Message {
            role: Role::Tool,
            content: content.to_string(),
            name: None,
            tool_calls: None,
            tool_call_id: Some(id.to_string()),
            reasoning_content: None,
        }
    }

    fn assistant_with_calls(calls: Vec<(String, (String, String))>) -> Message {
        Message {
            role: Role::Assistant,
            content: String::new(),
            name: None,
            tool_calls: Some(
                calls
                    .into_iter()
                    .map(|(id, (name, args))| ToolCall {
                        id,
                        ty: "function".to_string(),
                        function: FunctionCall {
                            name,
                            arguments: args,
                        },
                    })
                    .collect(),
            ),
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    #[test]
    fn diagnose_report_serde_roundtrip() {
        let report = DiagnoseReport {
            session_id: "s-1".into(),
            outcome: "paused".into(),
            phases: vec![PhaseSpan {
                name: "plan".into(),
                started_at_ms: 0,
                ended_at_ms: 10,
                duration_ms: 10,
            }],
            failures: vec![FailureDetail {
                phase: "verify".into(),
                tool: Some("bash".into()),
                command: Some("cargo check".into()),
                error: "exit 1".into(),
                root_cause: Some("bad import".into()),
                fix_plan: Some("fix import".into()),
            }],
            sub_agents: vec![SubAgentSpan {
                preset: "explorer".into(),
                outcome: "success".into(),
                duration_ms: 0,
            }],
            quality: vec![QualityFinding {
                rule: "no-commit-secret".into(),
                severity: deepseeknova_core::tool_hook::FindingSeverity::Warning,
                passed: false,
                evidence: "-----BEGIN".into(),
            }],
            adversarial_review: None,
            generated_at_ms: 1234,
        };
        let json = serde_json::to_string(&report).unwrap();
        let back: DiagnoseReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, report);
        assert!(json.contains("\"session_id\":\"s-1\""));

        // 旧报告（无 adversarial_review 字段）反序列化兼容 → None
        let legacy = r#"{"session_id":"s-1","outcome":"paused","phases":[],"failures":[],"sub_agents":[],"quality":[],"generated_at_ms":1}"#;
        let back: DiagnoseReport = serde_json::from_str(legacy).unwrap();
        assert_eq!(back.adversarial_review, None);
    }

    #[test]
    fn diagnose_report_new_builds_empty() {
        let r = DiagnoseReport::new("s-2", "failed");
        assert_eq!(r.session_id, "s-2");
        assert_eq!(r.outcome, "failed");
        assert!(r.phases.is_empty());
        assert!(r.failures.is_empty());
        assert!(r.sub_agents.is_empty());
        assert!(r.quality.is_empty());
        assert!(r.generated_at_ms > 0);
    }

    #[test]
    fn phase_spans_close_and_stay_monotonic() {
        let mut c = DiagnoseCollector::new();
        c.phase_enter("plan");
        // 幂等：同名重复进入不重置起点。
        c.phase_enter("plan");
        std::thread::sleep(std::time::Duration::from_millis(2));
        c.phase_enter("tool");
        std::thread::sleep(std::time::Duration::from_millis(2));
        c.phase_enter("verify");
        let report = c.build_report("s".into(), "paused".into(), Vec::new(), None);
        let names: Vec<&str> = report.phases.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["plan", "tool", "verify"]);
        for p in &report.phases {
            assert!(
                p.ended_at_ms >= p.started_at_ms,
                "phase {} ended < started",
                p.name
            );
            assert_eq!(p.duration_ms, p.ended_at_ms - p.started_at_ms);
        }
        // 起点依次递增（闭合逻辑正确）。
        for w in report.phases.windows(2) {
            assert!(w[1].started_at_ms >= w[0].ended_at_ms);
        }
    }

    #[test]
    fn failure_detail_uses_last_reflection() {
        let mut c = DiagnoseCollector::new();
        c.record_reflection(Reflection {
            root_cause: "rc".into(),
            fix_plan: "fp".into(),
            lesson: "l".into(),
        });
        c.record_failure("verify", Some("bash".into()), None, "boom".into());
        let report = c.build_report("s".into(), "paused".into(), Vec::new(), None);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].root_cause.as_deref(), Some("rc"));
        assert_eq!(report.failures[0].fix_plan.as_deref(), Some("fp"));
        assert_eq!(report.failures[0].phase, "verify");
    }

    #[test]
    fn collect_tool_failures_and_sub_agents_from_history() {
        let msgs = vec![
            assistant_with_calls(vec![
                ("c1".into(), ("read_file".into(), "{}".into())),
                (
                    "c2".into(),
                    (
                        "delegate".into(),
                        r#"{"agent":"explorer","goal":"find"}"#.into(),
                    ),
                ),
            ]),
            tool_msg("c1", "Error: boom"),
            tool_msg("c2", "[delegate:explorer] found it"),
            assistant_with_calls(vec![(
                "c3".into(),
                ("bash".into(), r#"{"command":"cargo check"}"#.into()),
            )]),
            tool_msg("c3", "Error: exit 1"),
        ];
        let failures = collect_tool_failures(&msgs);
        assert_eq!(failures.len(), 2);
        assert_eq!(failures[0].tool.as_deref(), Some("read_file"));
        assert_eq!(failures[0].command, None);
        assert_eq!(failures[1].tool.as_deref(), Some("bash"));
        assert_eq!(failures[1].command.as_deref(), Some("cargo check"));
        assert!(failures[0].error.contains("boom"));

        let subs = collect_sub_agents(&msgs);
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].preset, "explorer");
        assert_eq!(subs[0].outcome, "success");
        assert_eq!(subs[0].duration_ms, 0);
    }

    #[test]
    fn sub_agent_outcome_heuristics() {
        let ok = vec![
            assistant_with_calls(vec![(
                "a".into(),
                ("delegate".into(), r#"{"agent":"coder"}"#.into()),
            )]),
            tool_msg("a", "[delegate:coder] done"),
        ];
        assert_eq!(collect_sub_agents(&ok)[0].outcome, "success");

        let failed = vec![
            assistant_with_calls(vec![(
                "b".into(),
                ("delegate".into(), r#"{"agent":"coder"}"#.into()),
            )]),
            tool_msg("b", "delegate to 'coder' failed: timeout"),
        ];
        assert_eq!(collect_sub_agents(&failed)[0].outcome, "failed");

        let aborted = vec![
            assistant_with_calls(vec![(
                "c".into(),
                ("delegate".into(), r#"{"agent":"coder"}"#.into()),
            )]),
            tool_msg("c", "Error: cancelled"),
        ];
        assert_eq!(collect_sub_agents(&aborted)[0].outcome, "aborted");
    }

    // -----------------------------------------------------------------------
    // F6：落盘脱敏 + 0600 权限
    // -----------------------------------------------------------------------

    fn secret_report() -> DiagnoseReport {
        DiagnoseReport {
            session_id: "s-secret".into(),
            outcome: "failed".into(),
            phases: Vec::new(),
            failures: vec![FailureDetail {
                phase: "tool".into(),
                tool: Some("bash".into()),
                command: Some("echo AKIAIOSFODNN7EXAMPLE > /tmp/k.pem; openssl genrsa".into()),
                error: "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAK...".into(),
                root_cause: None,
                fix_plan: None,
            }],
            sub_agents: Vec::new(),
            quality: vec![QualityFinding {
                rule: "no-commit-secret".into(),
                severity: deepseeknova_core::tool_hook::FindingSeverity::Blocking,
                passed: false,
                evidence: "-----BEGIN RSA PRIVATE KEY----- AKIAIOSFODNN7EXAMPLE".into(),
            }],
            adversarial_review: Some("AKIAIOSFODNN7EXAMPLE in bash output".into()),
            generated_at_ms: 1,
        }
    }

    #[test]
    fn write_to_redacts_secrets_and_uses_0600_permissions() {
        let dir = std::env::temp_dir().join(format!(
            "dnv-diag-write-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let report = secret_report();
        let path = report.write_to(&dir).expect("write_to must succeed");
        let raw = std::fs::read_to_string(&path).expect("file must be readable");
        // 密钥串被替换，不留原文。
        assert!(
            !raw.contains("AKIAIOSFODNN7EXAMPLE"),
            "AWS key must be redacted"
        );
        assert!(
            !raw.contains("PRIVATE KEY-----"),
            "PEM header must be redacted"
        );
        assert!(raw.contains("[REDACTED]"), "redaction marker must appear");
        // 明文保留（非密钥内容不受影响）。
        assert!(raw.contains("openssl genrsa"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "report file must be 0600, got {mode:o}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn redacted_keeps_safe_fields_intact() {
        let report = secret_report();
        let r = report.redacted();
        assert_eq!(r.failures[0].tool.as_deref(), Some("bash"));
        assert_eq!(r.failures[0].phase, "tool");
        assert!(!r.failures[0].error.contains("PRIVATE KEY"));
        assert!(r.failures[0].error.contains("[REDACTED]"));
        assert!(!r.failures[0].command.as_deref().unwrap().contains("AKIA"));
        assert!(!r.quality[0].evidence.contains("AKIA"));
    }

    // -----------------------------------------------------------------------
    // F7：Drop 兜底相位推导
    // -----------------------------------------------------------------------

    #[test]
    fn fallback_failure_phase_reflects_collector_state() {
        // 未进入任何相位 → plan。
        let mut c = DiagnoseCollector::new();
        c.phase_enter("plan");
        assert_eq!(c.fallback_failure_phase(), "plan");
        // 进入 tool 相位后异常终止 → tool。
        let mut c = DiagnoseCollector::new();
        c.phase_enter("plan");
        c.phase_enter("tool");
        assert_eq!(c.fallback_failure_phase(), "tool");
        // tool 相位闭合后再进入 reflect → 如实填 reflect。
        let mut c = DiagnoseCollector::new();
        c.phase_enter("plan");
        c.phase_enter("tool");
        c.phase_enter("reflect");
        assert_eq!(c.fallback_failure_phase(), "reflect");
        // tool 已闭合、当前为 verify（非 reflect/review）→ 进入过 tool → tool。
        let mut c = DiagnoseCollector::new();
        c.phase_enter("plan");
        c.phase_enter("tool");
        c.phase_enter("verify");
        assert_eq!(c.fallback_failure_phase(), "tool");
        // 仅 plan（未执行工具）→ plan。
        let mut c = DiagnoseCollector::new();
        c.phase_enter("plan");
        assert_eq!(c.fallback_failure_phase(), "plan");
    }
}
