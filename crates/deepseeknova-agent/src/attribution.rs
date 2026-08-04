//! 失败归因：子代理/验证失败时的 LLM 判定 → Retry / Degrade / Abort。
//!
//! 与 reflection 同哲学：调用失败或响应不可解析一律返回 `None`（调用方按
//! `Abort` 处理，不阻塞、不猜）；JSON 契约在 reflection 已验证的
//! `{root_cause, fix_plan, lesson}` 基础上扩展为
//! `{"root_cause":"...","verdict":"retry|degrade|abort","fix_plan":"..."}`。
//! 归因调用受硬预算约束（防烧 token），超限后由调用方走盲重试/直接上抛。

use deepseeknova_core::{Message, Role};
use deepseeknova_provider::{Provider, ValidatedRequest};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tracing::warn;

/// 单次 run 内归因调用次数硬上限（防烧 token；超限走盲重试/上抛兜底）。
/// 默认值先为 agent 内常量，config 键由收尾父级经 runtime 装配。
pub const MAX_ATTRIBUTIONS_PER_RUN: usize = 3;

/// 归因输入的失败文本截断上限（字符），防止错误文本撑爆归因 prompt。
pub(crate) const MAX_ATTRIBUTION_INPUT_CHARS: usize = 2000;

/// 归因判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 追加 root_cause/fix_plan 反馈后重试同一委派。
    Retry,
    /// 换预设/换路径重试（degrade_map 未映射时按 Retry 处理）。
    Degrade,
    /// 重试无意义，直接上抛。
    Abort,
}

/// 一次失败归因的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct Attribution {
    pub root_cause: String,
    pub verdict: Verdict,
    /// 修复计划（Abort 判定时可能缺失）。
    pub fix_plan: Option<String>,
}

/// 归因设置（runtime 装配；agent crate 不依赖 config，预算默认用常量）。
#[derive(Clone)]
pub struct AttributionSettings {
    /// 归因 LLM（独立于主 provider，成本可控）。
    pub provider: Arc<dyn Provider>,
    /// Retry/Degrade 路径的重试次数上限（含首次失败后的重试次数）。
    pub max_retries: usize,
    /// 归因调用次数上限（跨委派调用累计，防烧 token）。
    pub max_attributions: usize,
    /// Degrade 时的降级目标预设映射（agent 名 → 目标 preset 名）；
    /// 未映射的 agent 判定 Degrade 时按 Retry 处理（同反馈重试）。
    pub degrade_map: HashMap<String, String>,
}

/// 归因预算门卫：原子计数，未超限时 `try_consume` 返回 true。
pub struct AttributionBudget {
    used: AtomicUsize,
    max: usize,
}

impl AttributionBudget {
    pub fn new(max: usize) -> Self {
        Self {
            used: AtomicUsize::new(0),
            max,
        }
    }

    /// 尝试消耗一次预算；未超限返回 true（超限后不再消耗）。
    pub fn try_consume(&self) -> bool {
        self.used
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |u| {
                (u < self.max).then_some(u + 1)
            })
            .is_ok()
    }

    /// 已消耗的归因次数（诊断用）。
    pub fn used(&self) -> usize {
        self.used.load(Ordering::SeqCst)
    }
}

/// 渲染归因 prompt：任务 + 失败摘要 → 严格要求 JSON 判定。
pub fn render_attribution_prompt(task: &str, failure: &str) -> String {
    format!(
        "You are diagnosing a failed delegated execution. Determine whether \
         retrying can plausibly succeed and what to change. Respond with ONLY a \
         JSON object: {{\"root_cause\": \"...\", \"verdict\": \"retry\" | \
         \"degrade\" | \"abort\", \"fix_plan\": \"...\"}}. Use \"retry\" when \
         retrying with feedback about the root cause can plausibly succeed; \
         \"degrade\" when a different approach or role is needed; \"abort\" when \
         retrying is pointless.\n\n\
         # Task\n{task}\n\n# Failure\n{failure}"
    )
}

/// 宽松解析归因响应（复用 review 的 extract_json）：root_cause 缺失/空、
/// verdict 非法或 JSON 不可解析 → None（调用方按 Abort 兜底）。
pub fn parse_attribution(raw: &str) -> Option<Attribution> {
    let json_str = crate::review::extract_json(raw)?;
    let v: serde_json::Value = serde_json::from_str(&json_str).ok()?;
    let root_cause = v.get("root_cause")?.as_str()?.trim();
    if root_cause.is_empty() {
        return None;
    }
    let verdict = match v
        .get("verdict")?
        .as_str()?
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "retry" => Verdict::Retry,
        "degrade" => Verdict::Degrade,
        "abort" => Verdict::Abort,
        _ => return None,
    };
    let fix_plan = v
        .get("fix_plan")
        .and_then(|f| f.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Some(Attribution {
        root_cause: root_cause.to_string(),
        verdict,
        fix_plan,
    })
}

/// 单次归因调用：failure 按 `max_chars` 截断；失败/不可解析返回 None。
pub async fn run_attribution(
    provider: &dyn Provider,
    task: &str,
    failure: &str,
    max_chars: usize,
) -> Option<Attribution> {
    let failure_capped: String = failure.chars().take(max_chars).collect();
    let prompt = render_attribution_prompt(task, &failure_capped);
    let msgs = vec![Message {
        role: Role::User,
        content: prompt,
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    }];
    let validated = match ValidatedRequest::new(&msgs, &[]) {
        Ok(v) => v,
        Err(violations) => {
            warn!(
                "invalid attribution request ({}); skipping",
                violations.join("; ")
            );
            return None;
        }
    };
    match provider.generate(validated).await {
        Ok(out) => match parse_attribution(&out.content) {
            Some(a) => Some(a),
            None => {
                warn!("attribution response unparseable; defaulting to abort");
                None
            }
        },
        Err(e) => {
            warn!("attribution call failed ({e}); defaulting to abort");
            None
        }
    }
}

/// 重试反馈消息：错误 + 归因（根因 + 修复计划）。与 verify 回炉的
/// "反馈追加"模式一致：反馈在前，调用方把原文案（goal）拼在尾部。
pub fn compose_retry_feedback(error: &str, a: &Attribution) -> String {
    let mut out = format!(
        "[delegate failure attribution]\nroot cause: {}\n",
        a.root_cause
    );
    if let Some(plan) = a.fix_plan.as_deref().filter(|p| !p.is_empty()) {
        out.push_str(&format!("fix plan: {plan}\n"));
    }
    out.push_str("\nPrevious attempt error:\n");
    out.push_str(error);
    out
}

/// core `AttributionHook` 的同步适配：图节点失败 → 确定性错误摘要。
///
/// core 侧 hook 是同步记录型接口（节点重试耗尽后调用，无法 await LLM）；
/// 真正的 LLM 归因已由 DelegateEngine / agent 主循环内部完成（本模块
/// `run_attribution`）。此适配把节点失败文本截断为摘要，供日志与下游
/// 消费（root cause 级信息），由 runtime/coordinator 收尾接线。
pub struct NodeFailureSummary;

impl deepseeknova_core::executor::AttributionHook for NodeFailureSummary {
    fn on_node_failure(
        &self,
        node_id: &deepseeknova_core::graph::NodeId,
        error: &deepseeknova_core::graph::NodeOutput,
    ) -> Option<String> {
        match error {
            deepseeknova_core::graph::NodeOutput::Error(e) => {
                let capped: String = e.chars().take(MAX_ATTRIBUTION_INPUT_CHARS).collect();
                Some(format!("node {node_id} failed: {capped}"))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribution_prompt_contains_contract_and_sections() {
        let p = render_attribution_prompt("fix auth", "verify failed");
        for s in [
            "# Task",
            "# Failure",
            "\"root_cause\"",
            "\"verdict\"",
            "retry",
            "degrade",
            "abort",
        ] {
            assert!(p.contains(s), "prompt 缺少 {s}");
        }
    }

    #[test]
    fn parses_attribution_verdicts() {
        let retry = parse_attribution(
            r#"{"root_cause":"missing import","verdict":"retry","fix_plan":"add the import"}"#,
        )
        .unwrap();
        assert_eq!(retry.root_cause, "missing import");
        assert_eq!(retry.verdict, Verdict::Retry);
        assert_eq!(retry.fix_plan.as_deref(), Some("add the import"));

        let degrade =
            parse_attribution(r#"{"root_cause":"read-only","verdict":"degrade"}"#).unwrap();
        assert_eq!(degrade.verdict, Verdict::Degrade);
        assert_eq!(degrade.fix_plan, None, "fix_plan 可缺失");

        let abort = parse_attribution(
            "Here:\n```json\n{\"root_cause\":\"impossible\",\"verdict\":\"ABORT\"}\n```",
        )
        .unwrap();
        assert_eq!(abort.verdict, Verdict::Abort, "verdict 大小写不敏感");

        // verdict 为空串 → 视为缺失（不猜）
        assert_eq!(
            parse_attribution(r#"{"root_cause":"x","verdict":""}"#),
            None
        );
    }

    #[test]
    fn garbage_and_missing_fields_yield_none() {
        assert_eq!(parse_attribution("not json"), None);
        assert_eq!(parse_attribution(r#"{"root_cause":"a"}"#), None);
        assert_eq!(
            parse_attribution(r#"{"root_cause":"","verdict":"retry"}"#),
            None
        );
        assert_eq!(
            parse_attribution(r#"{"root_cause":"a","verdict":"maybe"}"#),
            None
        );
    }

    #[test]
    fn budget_consumes_until_max() {
        let b = AttributionBudget::new(2);
        assert!(b.try_consume());
        assert!(b.try_consume());
        assert!(!b.try_consume(), "超限后不再消耗");
        assert_eq!(b.used(), 2);

        let zero = AttributionBudget::new(0);
        assert!(!zero.try_consume(), "上限 0 = 归因关闭");
    }

    #[test]
    fn compose_retry_feedback_prepends_attribution_and_keeps_error() {
        let a = Attribution {
            root_cause: "bad import".into(),
            verdict: Verdict::Retry,
            fix_plan: Some("fix the import".into()),
        };
        let msg = compose_retry_feedback("boom: exit 1", &a);
        assert!(msg.contains("[delegate failure attribution]"));
        assert!(msg.contains("root cause: bad import"));
        assert!(msg.contains("fix plan: fix the import"));
        assert!(msg.ends_with("boom: exit 1"), "错误必须保留在尾部");

        // fix_plan 缺失时反馈不出现 fix plan 行
        let no_plan = Attribution {
            root_cause: "x".into(),
            verdict: Verdict::Abort,
            fix_plan: None,
        };
        let msg = compose_retry_feedback("err", &no_plan);
        assert!(!msg.contains("fix plan:"));
        assert!(msg.contains("root cause: x"));
    }

    struct FixedProvider {
        content: String,
        fail: bool,
    }

    #[async_trait::async_trait]
    impl Provider for FixedProvider {
        async fn generate(&self, _validated: ValidatedRequest<'_>) -> anyhow::Result<Message> {
            if self.fail {
                anyhow::bail!("provider down");
            }
            Ok(Message {
                role: Role::Assistant,
                content: self.content.clone(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            })
        }
    }

    #[tokio::test]
    async fn run_attribution_routes_success_failure_and_garbage() {
        let ok = FixedProvider {
            content: r#"{"root_cause":"a","verdict":"retry","fix_plan":"b"}"#.into(),
            fail: false,
        };
        let a = run_attribution(&ok, "task", "failed", 4000).await.unwrap();
        assert_eq!(a.root_cause, "a");
        assert_eq!(a.verdict, Verdict::Retry);

        let down = FixedProvider {
            content: String::new(),
            fail: true,
        };
        assert_eq!(run_attribution(&down, "task", "failed", 4000).await, None);

        let garbage = FixedProvider {
            content: "I'll fix it".into(),
            fail: false,
        };
        assert_eq!(
            run_attribution(&garbage, "task", "failed", 4000).await,
            None
        );
    }
}
