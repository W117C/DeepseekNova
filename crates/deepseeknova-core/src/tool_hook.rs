//! 工具生命周期钩子（任务质量闭环 A 阶段：ToolHook 链）。
//!
//! 钩子在工具调用前后被 agent 主循环调用：`before` 返回放行/询问/拒绝
//! 决策，`after` 对工具结果文本做写后策略评估并产出 [`QualityFinding`]。
//! panic 契约（对齐 harness 插件 fail-open 原则，安全判定例外）：
//! `before`/`interested` panic 按 [`HookVerdict::Deny`] 处理（安全判定
//! fail-closed），`after` panic 按空 findings 处理（fail-open，不阻断执行）。

use crate::types::ToolCall;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// 钩子对一次工具调用的放行决策。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HookVerdict {
    /// 放行。
    Allow,
    /// 放行并附带说明（本阶段仅记录语义，不改变执行）。
    AllowWith(String),
    /// 需要用户确认；由调用方走 approval 桥（与 permission gate 的 Ask 同路径）。
    Ask(String),
    /// 拒绝执行，附拒绝原因。
    Deny(String),
}

/// 质量 finding 的严重级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FindingSeverity {
    /// 仅记录，不阻断。
    Info,
    /// 警告，不阻断但应关注。
    Warning,
    /// 阻断级：置位会话 blocking 标志（B3 review 短路的触发条件）。
    Blocking,
}

/// 一条质量策略评估结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityFinding {
    /// 命中/评估的规则 id（如 `no-commit-secret`）。
    pub rule: String,
    /// 严重级别。
    pub severity: FindingSeverity,
    /// `false` = 违规（命中规则）；`true` = 仅审计用（本阶段未使用）。
    pub passed: bool,
    /// 命中摘要（正则命中片段 / 违规路径 / 字节数等）。
    pub evidence: String,
}

/// 传给钩子方法的只读上下文（最小化：仅工作区根）。
#[derive(Debug, Clone, Copy)]
pub struct ToolHookCtx<'a> {
    /// 工作区根目录，用于解析相对路径。
    pub workspace_root: &'a Path,
}

/// 工具生命周期钩子。实现须 `Send + Sync`；所有方法为同步调用，
/// 由 agent 主循环在 await 点之间串行执行。
///
/// panic 契约：实现 panic 时调用方以 `catch_unwind` 捕获——`before` 与
/// `interested` 按 [`HookVerdict::Deny`] 处理（安全判定 fail-closed，warn
/// 注明），`after` 按空 findings 处理（fail-open，不阻断执行）。
pub trait ToolHook: Send + Sync {
    /// 钩子名称（日志/诊断用）。
    fn name(&self) -> &str;

    /// 是否对本次调用感兴趣。默认对所有调用感兴趣。
    fn interested(&self, _call: &ToolCall) -> bool {
        true
    }

    /// 工具执行前的预检。默认放行。
    fn before(&self, _ctx: &ToolHookCtx, _call: &ToolCall) -> HookVerdict {
        HookVerdict::Allow
    }

    /// 工具执行成功后的写后评估。默认无 findings。
    fn after(&self, _ctx: &ToolHookCtx, _call: &ToolCall, _result: &str) -> Vec<QualityFinding> {
        Vec::new()
    }
}

/// 空实现：全放行、零 findings。用作默认/测试桩。
pub struct NoopToolHook;

impl ToolHook for NoopToolHook {
    fn name(&self) -> &str {
        "noop"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_verdict_serde_roundtrip() {
        for verdict in [
            HookVerdict::Allow,
            HookVerdict::AllowWith("note".to_string()),
            HookVerdict::Ask("confirm?".to_string()),
            HookVerdict::Deny("blocked".to_string()),
        ] {
            let json = serde_json::to_string(&verdict).unwrap();
            let back: HookVerdict = serde_json::from_str(&json).unwrap();
            assert_eq!(back, verdict);
        }
    }

    #[test]
    fn quality_finding_serde_roundtrip() {
        let f = QualityFinding {
            rule: "no-commit-secret".to_string(),
            severity: FindingSeverity::Blocking,
            passed: false,
            evidence: "-----BEGIN RSA PRIVATE KEY-----".to_string(),
        };
        let json = serde_json::to_string(&f).unwrap();
        let back: QualityFinding = serde_json::from_str(&json).unwrap();
        assert_eq!(back, f);
        assert!(json.contains("\"severity\":\"Blocking\""));
    }

    #[test]
    fn finding_severity_serde_roundtrip() {
        for s in [
            FindingSeverity::Info,
            FindingSeverity::Warning,
            FindingSeverity::Blocking,
        ] {
            let json = serde_json::to_string(&s).unwrap();
            let back: FindingSeverity = serde_json::from_str(&json).unwrap();
            assert_eq!(back, s);
        }
    }

    #[test]
    fn noop_hook_defaults_allow_and_empty_findings() {
        let hook = NoopToolHook;
        assert_eq!(hook.name(), "noop");
        let call = ToolCall {
            id: "call_1".into(),
            ty: "function".into(),
            function: crate::types::FunctionCall {
                name: "write_file".into(),
                arguments: "{}".into(),
            },
        };
        let ctx = ToolHookCtx {
            workspace_root: std::path::Path::new("/tmp"),
        };
        // 默认实现：interested = true、before = Allow、after = 空 findings。
        assert!(hook.interested(&call));
        assert_eq!(hook.before(&ctx, &call), HookVerdict::Allow);
        assert!(hook.after(&ctx, &call, "ok").is_empty());
    }
}
