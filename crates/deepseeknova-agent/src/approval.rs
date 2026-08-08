//! 审批（Ask 裁决）相关渲染辅助：拒绝即教育建议渲染 + 风险前缀。
//!
//! 从 `agent.rs` 拆分（M7）：本模块保持纯搬移，不改行为/签名/逻辑。

use deepseeknova_permission::{PermissionGate, RuleSuggestion};

/// 将权限裁决的"拒绝即教育"建议渲染为人类可读文本；无建议时返回空串。
pub(crate) fn render_suggestions(suggestions: &[RuleSuggestion]) -> String {
    if suggestions.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = suggestions
        .iter()
        .map(|s| {
            let rule = match s.rule.subject {
                Some(ref sub) => format!("{} subject={}", s.rule.tool, sub),
                None => s.rule.tool.clone(),
            };
            format!(
                "[建议] 添加规则即可自动放行: behavior={:?} rule={rule}",
                s.behavior
            )
        })
        .collect();
    lines.join("\n")
}

/// Ask 审批描述的风险前缀（观测台规范：只读 / 非只读 / 危险）。
/// 非 shell 工具或参数不可解析时返回 `None`（保持旧描述不变）。
pub(crate) fn approval_risk_prefix(
    gate: Option<&PermissionGate>,
    tool_name: &str,
    args: &str,
) -> Option<String> {
    let kind = gate?.shell_readonly_kind(tool_name, args)?;
    let label = match kind {
        deepseeknova_security::readonly::ReadOnlyKind::ReadOnly => "只读",
        deepseeknova_security::readonly::ReadOnlyKind::NotReadOnly => "非只读",
        deepseeknova_security::readonly::ReadOnlyKind::Dangerous => "危险",
    };
    Some(format!("[风险:{label}]"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_permission::{Decision, PermissionGate, Policy};

    #[test]
    fn approval_risk_prefix_maps_readonly_kinds() {
        let gate = PermissionGate::new(Policy {
            mode: Decision::Ask,
            allow: vec![],
            ask: vec![],
            deny: vec![],
        });
        assert_eq!(
            approval_risk_prefix(Some(&gate), "bash", r#"{"command": "git status"}"#).as_deref(),
            Some("[风险:只读]")
        );
        assert_eq!(
            approval_risk_prefix(Some(&gate), "Bash", r#"{"command": "rm -rf /tmp/x"}"#).as_deref(),
            Some("[风险:非只读]")
        );
        assert_eq!(
            approval_risk_prefix(
                Some(&gate),
                "shell",
                r#"{"command": "git -c core.pager='sh -x' status"}"#
            )
            .as_deref(),
            Some("[风险:危险]")
        );
        assert_eq!(
            approval_risk_prefix(Some(&gate), "grep", r#"{"command": "x"}"#),
            None
        );
        assert_eq!(
            approval_risk_prefix(None, "bash", r#"{"command": "x"}"#),
            None
        );
    }
}
