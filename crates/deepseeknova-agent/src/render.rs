//! 渲染辅助：回炉文案 / 对抗审查证据 / 观察压缩提示词。
//!
//! 从 `agent.rs` 拆分（M7）：本模块保持纯搬移，不改行为/签名/逻辑。

use deepseeknova_core::tool_hook::{FindingSeverity, QualityFinding};
use deepseeknova_core::{Message, Role};

/// Verify 失败回炉文案（契约：标记 + 原因 + 修复后重跑验证的语义）。
pub(crate) fn verify_failure_message(reason: &str) -> String {
    format!(
        "[verification failed]\n{reason}\n\nFix the issues, then finish the task. \
         The verification commands will run again before completion."
    )
}

/// 审查输入证据预算（字符上限；超出截断）。
const ADVERSARIAL_REVIEW_INPUT_CAP_CHARS: usize = 6000;

/// 渲染审查输入证据（任务 + findings + 工具调用摘要，字符预算截断）。
pub(crate) fn render_adversarial_evidence(
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

/// Observe 阶段工具输出压缩提示词（契约：保留事实/路径/退出码/数字，纯摘要输出）。
pub(crate) fn render_compression_prompt(tool: &str, raw: &str) -> String {
    format!(
        "You are the Observe stage of the Observe → Plan → Tool → Verify → \
         Reflect → Next Action loop. Compress the following tool output \
         (`{tool}`) into a concise structured summary. Preserve every fact, \
         file path, exit code and number. Output only the summary.\n\n{raw}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
