//! DNA 五阶段协议增强：阶段门控类型、门控违规记录与阶段迁移事件。
//!
//! 本模块提供协议执行引擎（设计 A）的 core 侧类型：DNA 五阶段
//! ([`Phase`])、门控违规 ([`GateViolation`])、阶段迁移 ([`PhaseTransition`] /
//! [`PhaseOutcome`])、drift 检测产出 ([`DriftFinding`])，以及协议门控 trait
//! ([`PhaseGate`]) 与无门控默认实现 ([`NoopPhaseGate`])。
//!
//! [`FindingSeverity`] 与 [`QualityFinding`] 复用 `tool_hook` 模块的既有类型。

use crate::tool_hook::{FindingSeverity, QualityFinding};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;

/// DNA 五阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// 理解任务与约束。
    Understand,
    /// 制定执行计划。
    Plan,
    /// 执行计划。
    Execute,
    /// 验证结果。
    Verify,
    /// 蒸馏产出与知识回灌。
    Distill,
}

/// 门控违规记录（进事件流 + 评分卡消费）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateViolation {
    /// 违规的门名称（如 `plan-before-execute`）。`Cow<'static, str>` 使派生
    /// Deserialize 无需 `'de: 'static` 约束，且反序列化不会泄漏内存
    /// （门名短，构造处 `Cow::Borrowed`，反序列化为 Owned）。
    pub gate: Cow<'static, str>,
    /// 违规发生的阶段。
    pub phase: Phase,
    /// 违规严重级别（Info/Warning/Blocking）。
    pub severity: FindingSeverity,
    /// 违规详情。
    pub detail: String,
}

/// 阶段迁移事件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseTransition {
    /// 迁移到的阶段。
    pub phase: Phase,
    /// 该阶段迁移的结果。
    pub outcome: PhaseOutcome,
}

/// 阶段迁移结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseOutcome {
    /// 门控通过。
    Pass,
    /// 门控跳过（未启用/未配置）。
    Skipped,
    /// 门控违规。
    Violated,
}

/// Execute 阶段 drift 检测产出（失败路径重复）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriftFinding {
    /// 失败重复的工具族（如 "bash"）。
    pub tool_family: String,
    /// 连续失败次数。
    pub failures: u32,
    /// 失败详情。
    pub detail: String,
}

/// 协议门控 trait（core 定义，agent 实现，runtime 装配——ToolHook 先例）。
pub trait PhaseGate: Send + Sync {
    /// 门名称（日志/诊断用）。
    fn name(&self) -> &'static str;

    /// 对一次阶段迁移/阶段内检查求值；空 = 通过。
    fn check(&self, ctx: &PhaseGateCtx) -> Vec<GateViolation>;
}

/// 门控上下文：由 agent PhaseRunner 构造（阶段名、事件摘要、verify 配置、窗口统计）。
pub struct PhaseGateCtx {
    /// 当前阶段。
    pub phase: Phase,
    /// verify 是否已配置。
    pub verify_configured: bool,
    /// Verification(passed=true) 事件数。
    pub verify_passed_count: u32,
    /// Verification(passed=false) 事件数。
    pub verify_failed_count: u32,
    /// 工具调用次数。
    pub tool_calls: u32,
    /// 连续失败计数（滑动窗口）：工具族 → 失败次数。
    pub tool_failures_by_family: HashMap<String, u32>,
    /// 当前阶段内的质量 findings。
    pub findings: Vec<QualityFinding>,
    /// Distill 阶段是否已产出 lesson（memory_distill/reflection 结果）。
    pub has_lesson: bool,
    /// 会话内是否已产出计划性文本（plan-before-execute 门数据通道；
    /// 阶段3 由 agent PhaseRunner 维护）。
    pub has_plan_text: bool,
    /// 运行耗时（毫秒）。
    pub run_ms: u64,
}

/// 无门控注册时的默认实现（no-op，零成本）。
pub struct NoopPhaseGate;

impl PhaseGate for NoopPhaseGate {
    /// 返回固定名 `"noop"`。
    fn name(&self) -> &'static str {
        "noop"
    }

    /// 无门控：恒返回空违规列表。
    fn check(&self, _ctx: &PhaseGateCtx) -> Vec<GateViolation> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ctx() -> PhaseGateCtx {
        PhaseGateCtx {
            phase: Phase::Execute,
            verify_configured: false,
            verify_passed_count: 0,
            verify_failed_count: 0,
            tool_calls: 0,
            tool_failures_by_family: HashMap::new(),
            findings: Vec::new(),
            has_lesson: false,
            has_plan_text: false,
            run_ms: 0,
        }
    }

    #[test]
    fn phase_serde_roundtrip() {
        for phase in [
            Phase::Understand,
            Phase::Plan,
            Phase::Execute,
            Phase::Verify,
            Phase::Distill,
        ] {
            let json = serde_json::to_string(&phase).unwrap();
            let back: Phase = serde_json::from_str(&json).unwrap();
            assert_eq!(back, phase);
        }
    }

    #[test]
    fn phase_serializes_with_snake_case_tag() {
        assert_eq!(
            serde_json::to_string(&Phase::Understand).unwrap(),
            "\"understand\""
        );
        assert_eq!(
            serde_json::to_string(&Phase::Execute).unwrap(),
            "\"execute\""
        );
    }

    #[test]
    fn gate_violation_serde_roundtrip() {
        let v = GateViolation {
            gate: Cow::Borrowed("plan-before-execute"),
            phase: Phase::Execute,
            severity: FindingSeverity::Warning,
            detail: "no plan produced before first tool call".to_string(),
        };
        let json = serde_json::to_string(&v).unwrap();
        let back: GateViolation = serde_json::from_str(&json).unwrap();
        assert_eq!(serde_json::to_string(&back).unwrap(), json);
    }

    #[test]
    fn phase_transition_serde_roundtrip() {
        let t = PhaseTransition {
            phase: Phase::Verify,
            outcome: PhaseOutcome::Pass,
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: PhaseTransition = serde_json::from_str(&json).unwrap();
        assert_eq!(serde_json::to_string(&back).unwrap(), json);
    }

    #[test]
    fn drift_finding_serde_roundtrip() {
        let d = DriftFinding {
            tool_family: "bash".to_string(),
            failures: 3,
            detail: "same command failed 3 times in a row".to_string(),
        };
        let json = serde_json::to_string(&d).unwrap();
        let back: DriftFinding = serde_json::from_str(&json).unwrap();
        assert_eq!(serde_json::to_string(&back).unwrap(), json);
    }

    #[test]
    fn phase_outcome_serializes_with_snake_case_tag() {
        assert_eq!(
            serde_json::to_string(&PhaseOutcome::Pass).unwrap(),
            "\"pass\""
        );
        assert_eq!(
            serde_json::to_string(&PhaseOutcome::Skipped).unwrap(),
            "\"skipped\""
        );
        assert_eq!(
            serde_json::to_string(&PhaseOutcome::Violated).unwrap(),
            "\"violated\""
        );
    }

    #[test]
    fn noop_phase_gate_returns_empty_findings() {
        let gate = NoopPhaseGate;
        assert_eq!(gate.name(), "noop");
        assert!(gate.check(&sample_ctx()).is_empty());
    }

    #[test]
    fn wire_event_roundtrip_protocol_variants() {
        // WireEvent 样式：三个新变体序列化往返（含 kind 标签）。
        let transition = PhaseTransition {
            phase: Phase::Execute,
            outcome: PhaseOutcome::Pass,
        };
        let wire = crate::runner::WireEvent::PhaseTransition {
            transition: transition.clone(),
        };
        let json = serde_json::to_string(&wire).unwrap();
        assert!(
            json.contains("\"kind\":\"phase_transition\""),
            "json = {json}"
        );
        let back: crate::runner::WireEvent = serde_json::from_str(&json).unwrap();
        match back {
            crate::runner::WireEvent::PhaseTransition { transition: t } => {
                assert_eq!(t, transition);
            }
            other => panic!("expected PhaseTransition, got {other:?}"),
        }

        let violation = GateViolation {
            gate: Cow::Borrowed("verify-evidence"),
            phase: Phase::Verify,
            severity: FindingSeverity::Blocking,
            detail: "verify configured but zero passed".to_string(),
        };
        let wire = crate::runner::WireEvent::GateViolation {
            violation: violation.clone(),
        };
        let json = serde_json::to_string(&wire).unwrap();
        assert!(
            json.contains("\"kind\":\"gate_violation\""),
            "json = {json}"
        );
        let back: crate::runner::WireEvent = serde_json::from_str(&json).unwrap();
        match back {
            crate::runner::WireEvent::GateViolation { violation: v } => {
                assert_eq!(v.gate, violation.gate);
                assert_eq!(v.phase, violation.phase);
                assert_eq!(v.severity, violation.severity);
                assert_eq!(v.detail, violation.detail);
            }
            other => panic!("expected GateViolation, got {other:?}"),
        }

        let drift = DriftFinding {
            tool_family: "bash".to_string(),
            failures: 3,
            detail: "repeated failure".to_string(),
        };
        let wire = crate::runner::WireEvent::DriftFinding {
            drift: drift.clone(),
        };
        let json = serde_json::to_string(&wire).unwrap();
        assert!(json.contains("\"kind\":\"drift_finding\""), "json = {json}");
        let back: crate::runner::WireEvent = serde_json::from_str(&json).unwrap();
        match back {
            crate::runner::WireEvent::DriftFinding { drift: d } => assert_eq!(d, drift),
            other => panic!("expected DriftFinding, got {other:?}"),
        }
    }

    #[test]
    fn fake_phase_gate_reports_violation() {
        // 假门：tool_calls > 20 且无 lesson 时返回 Warning violation（distill-on-complex 语义）。
        struct DistillGate;
        impl PhaseGate for DistillGate {
            fn name(&self) -> &'static str {
                "distill-on-complex"
            }

            fn check(&self, ctx: &PhaseGateCtx) -> Vec<GateViolation> {
                if ctx.tool_calls > 20 && !ctx.has_lesson {
                    vec![GateViolation {
                        gate: Cow::Borrowed("distill-on-complex"),
                        phase: ctx.phase,
                        severity: FindingSeverity::Warning,
                        detail: "complex run without lesson".to_string(),
                    }]
                } else {
                    Vec::new()
                }
            }
        }

        let mut ctx = sample_ctx();
        ctx.phase = Phase::Distill;
        ctx.tool_calls = 21;
        ctx.has_lesson = false;

        let gate = DistillGate;
        let violations = gate.check(&ctx);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].gate, "distill-on-complex");
        assert_eq!(violations[0].phase, Phase::Distill);
        assert_eq!(violations[0].severity, FindingSeverity::Warning);

        // 通过场景：工具调用少 → 无违规。
        ctx.tool_calls = 5;
        assert!(gate.check(&ctx).is_empty());
        // 通过场景：已产出 lesson → 无违规。
        ctx.tool_calls = 21;
        ctx.has_lesson = true;
        assert!(gate.check(&ctx).is_empty());
    }
}
