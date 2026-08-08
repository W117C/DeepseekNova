//! DNA 五阶段协议门控运行器（协议增强能力包阶段3）。
//!
//! 职责：维护会话级阶段状态（当前 Phase、阶段/违规计数、verify 统计、
//! 工具失败滑动窗口），在阶段边界调用 [`PhaseGate`] 集合求值并合并违规，
//! 为评分卡 [`crate::agent::QualitySummary`] 提供 `protocol_violations` /
//! `phase_transitions` 统计。
//!
//! 内置门实现见 [`builtin_phase_gates`]；默认力度表见 spec §3.2
//! （plan-before-execute soft / verify-evidence hard / distill-on-complex
//! soft / drift-detection soft）。门注册为空（`with_protocol_gates` 未调用）
//! 时主循环走零成本路径，行为与现状完全一致。

use deepseeknova_core::protocol::{DriftFinding, GateViolation, Phase, PhaseGate, PhaseGateCtx};
use deepseeknova_core::tool_hook::{FindingSeverity, QualityFinding};
use std::borrow::Cow;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

/// 门控力度（runtime 配置 `[protocol] gates.<name>`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateLevel {
    /// 硬门：违规 severity 提升为 Blocking（拒绝/Ask 通道）。
    Hard,
    /// 软门：违规按门语义 severity（Warning/Info）进事件流。
    Soft,
    /// 关闭：门不产出任何违规。
    Off,
}

impl FromStr for GateLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "hard" => Ok(Self::Hard),
            "soft" => Ok(Self::Soft),
            "off" => Ok(Self::Off),
            other => Err(format!(
                "unknown gate level: '{other}' (expected hard|soft|off)"
            )),
        }
    }
}

/// 连续失败阈值（同工具族，滑动窗口，其间无成功）。
const DRIFT_THRESHOLD: u32 = 3;

/// 内置门默认力度表（spec §3.2）。
fn default_level(name: &str) -> GateLevel {
    match name {
        "verify-evidence" => GateLevel::Hard,
        _ => GateLevel::Soft,
    }
}

// ---------------------------------------------------------------------------
// 内置四门
// ---------------------------------------------------------------------------

/// 内置门统一载体：名称 + 力度 + 基础判定函数。
///
/// 力度语义（代码注释即 spec 偏差记录）：
/// - `Off` → 不产出任何违规；
/// - `Soft` → 保持门语义 severity（plan Warning / verify Blocking+Info /
///   distill Warning / drift Warning）；
/// - `Hard` → 门语义为 Warning 的违规提升为 Blocking（verify-evidence 的
///   Blocking 与 Info 保持：Info 是 bash 缺失降级通道，不因硬门升级阻断）。
struct BuiltinGate {
    name: &'static str,
    level: GateLevel,
    /// 基础判定：返回本门在 ctx 下的违规（无违规返回 None）。
    check_base: fn(&PhaseGateCtx) -> Option<GateViolation>,
}

impl PhaseGate for BuiltinGate {
    fn name(&self) -> &'static str {
        self.name
    }

    fn check(&self, ctx: &PhaseGateCtx) -> Vec<GateViolation> {
        if self.level == GateLevel::Off {
            return Vec::new();
        }
        let Some(mut v) = (self.check_base)(ctx) else {
            return Vec::new();
        };
        if self.level == GateLevel::Hard && v.severity == FindingSeverity::Warning {
            v.severity = FindingSeverity::Blocking;
        }
        vec![v]
    }
}

/// plan-before-execute：进 Execute 前无任何计划性文本 → Warning。
fn plan_before_execute(ctx: &PhaseGateCtx) -> Option<GateViolation> {
    if ctx.phase != Phase::Execute || ctx.has_plan_text {
        return None;
    }
    Some(GateViolation {
        gate: Cow::Borrowed("plan-before-execute"),
        phase: ctx.phase,
        severity: FindingSeverity::Warning,
        detail: "no plan text produced before first tool call".to_string(),
    })
}

/// verify-evidence（证据链判定，仅 Verify/Distill 阶段快照评估）：
/// - 未配置 → 通过；
/// - 已配置且有 passed 证据 → 通过；
/// - 已配置、零 passed 且有失败 → Blocking；
/// - 已配置、零 Verification 事件（bash 缺失/取消降级）→ Info。
fn verify_evidence(ctx: &PhaseGateCtx) -> Option<GateViolation> {
    if !matches!(ctx.phase, Phase::Verify | Phase::Distill) || !ctx.verify_configured {
        return None;
    }
    if ctx.verify_passed_count >= 1 {
        return None;
    }
    if ctx.verify_failed_count > 0 {
        Some(GateViolation {
            gate: Cow::Borrowed("verify-evidence"),
            phase: ctx.phase,
            severity: FindingSeverity::Blocking,
            detail: "verify configured but zero passed evidence".to_string(),
        })
    } else {
        Some(GateViolation {
            gate: Cow::Borrowed("verify-evidence"),
            phase: ctx.phase,
            severity: FindingSeverity::Info,
            detail: "verify configured but no verification events (bash missing or skipped)"
                .to_string(),
        })
    }
}

/// distill-on-complex：工具调用 >20 且未产出 lesson → Warning。
fn distill_on_complex(ctx: &PhaseGateCtx) -> Option<GateViolation> {
    if ctx.phase != Phase::Distill || ctx.tool_calls <= 20 || ctx.has_lesson {
        return None;
    }
    Some(GateViolation {
        gate: Cow::Borrowed("distill-on-complex"),
        phase: ctx.phase,
        severity: FindingSeverity::Warning,
        detail: "complex run (>20 tool calls) without lesson".to_string(),
    })
}

/// drift-detection：某工具族连续失败 ≥3 → Warning（DriftFinding 语义；
/// 实际 DriftFinding 事件由 [`PhaseRunner::note_tool_failure`] 产出）。
/// 同会话第二次同类 drift 的 Blocking 由 PhaseRunner 在 transition 时附加。
fn drift_detection(ctx: &PhaseGateCtx) -> Option<GateViolation> {
    if ctx.phase != Phase::Execute {
        return None;
    }
    let (family, &count) = ctx
        .tool_failures_by_family
        .iter()
        .find(|(_, &c)| c >= DRIFT_THRESHOLD)?;
    Some(GateViolation {
        gate: Cow::Borrowed("drift-detection"),
        phase: ctx.phase,
        severity: FindingSeverity::Warning,
        detail: format!("tool family '{family}' failed {count} times consecutively"),
    })
}

/// 构造四个内置门（缺省力度见 `default_level`；`levels` 覆盖同名门）。
///
/// Bugbot #4：`drift-detection=off` 时**从列表摘除** drift 门（而非保留一个
/// 空跑门）——PhaseRunner 以「gates 切片中是否存在 drift-detection 门」判定
/// drift 开关（`transition` 时探测），从而同步关闭 `note_tool_failure` 计数
/// 与二次 drift 附加。其余门 Off 时保留空跑（无行为差异，语义等价）。
pub fn builtin_phase_gates(levels: &HashMap<String, GateLevel>) -> Vec<Arc<dyn PhaseGate>> {
    let level = |name: &str| {
        levels
            .get(name)
            .copied()
            .unwrap_or_else(|| default_level(name))
    };
    let mut gates: Vec<Arc<dyn PhaseGate>> = vec![
        Arc::new(BuiltinGate {
            name: "plan-before-execute",
            level: level("plan-before-execute"),
            check_base: plan_before_execute,
        }),
        Arc::new(BuiltinGate {
            name: "verify-evidence",
            level: level("verify-evidence"),
            check_base: verify_evidence,
        }),
        Arc::new(BuiltinGate {
            name: "distill-on-complex",
            level: level("distill-on-complex"),
            check_base: distill_on_complex,
        }),
    ];
    if level("drift-detection") != GateLevel::Off {
        gates.push(Arc::new(BuiltinGate {
            name: "drift-detection",
            level: level("drift-detection"),
            check_base: drift_detection,
        }));
    }
    gates
}

// ---------------------------------------------------------------------------
// PhaseRunner — 会话级阶段状态机
// ---------------------------------------------------------------------------

/// 会话级阶段运行器（每个 `run_stream` 实例化一个，局部于主循环）。
pub struct PhaseRunner {
    /// 阶段迁移计数（QualitySummary.phase_transitions）。
    transitions: u32,
    /// 门控违规累计（QualitySummary.protocol_violations）。
    violations: u32,
    /// Verification(passed=true) 事件数。
    verify_passed: u32,
    /// Verification(passed=false) 事件数。
    verify_failed: u32,
    /// Verification 事件总数（含 passed/failed）。
    verify_events: u32,
    /// 工具调用总数。
    tool_calls: u32,
    /// 滑动窗口：工具族 → 连续失败计数（成功清零）。
    fail_streak: HashMap<String, u32>,
    /// 已报告 drift 的次数（family → 次数；第二次同类 → 附加 Warning「需人工
    /// 确认」）。Bugbot #5：只在阈值**首次**跨越时 +1（窗口未清零前不重复）。
    drift_reported: HashMap<String, u32>,
    /// drift 是否启用（Bugbot #4）：由 `transition` 在 gates 切片中探测
    /// `drift-detection` 门是否存在（Off 时 `builtin_phase_gates` 摘除该门）。
    /// false 时不计数、不发 DriftFinding、不附加二次 Warning。
    drift_enabled: bool,
    /// 会话内是否已产出计划性文本（单调置位）。
    has_plan_text: bool,
    /// Distill 阶段是否已产出 lesson。
    has_lesson: bool,
    /// run 起始时刻（ctx.run_ms 用）。
    run_started: std::time::Instant,
}

impl Default for PhaseRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl PhaseRunner {
    /// 新建运行器（以当前时刻为 run 起始零刻）。
    pub fn new() -> Self {
        Self {
            transitions: 0,
            violations: 0,
            verify_passed: 0,
            verify_failed: 0,
            verify_events: 0,
            tool_calls: 0,
            fail_streak: HashMap::new(),
            drift_reported: HashMap::new(),
            // drift 开关由 transition 探测 gates 切片；默认 true 覆盖「仅直接
            // 调用 note_tool_failure」的单元测试路径（agent 主循环总是先
            // transition(Execute) 再执行工具，探测必然先行）。
            drift_enabled: true,
            has_plan_text: false,
            has_lesson: false,
            run_started: std::time::Instant::now(),
        }
    }

    /// 推进到 `phase`：对全部门求值并合并违规。空违规 = 通过。
    ///
    /// 除门自身产出外，附加 drift 二次处理（Bugbot #3/#4/#5）：某工具族连续
    /// 失败 ≥3 且本会话已报告过该族 drift（[`Self::note_tool_failure`] 首次
    /// 跨越时计数）→ 在 **Execute** transition 时附加一条 Warning violation
    /// 并注明「需人工确认」。原设计为 Blocking/Ask 通道，但 Ask 桥是工具调用
    /// 级裁决（permission gate），drift 为阶段级无法复用（无 ApprovalRequest
    /// 事件、无 allow 分支，CLI 亦无 responder）——最小侵入降级为 Warning +
    /// 人工确认标注，与 spec §13 #2/#4 修正记录一致。仅在 drift 启用且
    /// `phase == Phase::Execute` 时附加（其他 phase 不附加，violations 不虚增）。
    /// 违规数计入 `stats()`。
    pub fn transition(
        &mut self,
        phase: Phase,
        gates: &[Arc<dyn PhaseGate>],
        ctx: &PhaseGateCtx,
    ) -> Vec<GateViolation> {
        self.transitions += 1;
        // Bugbot #4：drift 门存在 → 计数/附加启用（Off 时 builtin_phase_gates
        // 已摘除该门，此处探测得到 false）。
        self.drift_enabled = gates.iter().any(|g| g.name() == "drift-detection");
        let mut violations: Vec<GateViolation> = Vec::new();
        for gate in gates {
            violations.extend(gate.check(ctx));
        }
        if self.drift_enabled && phase == Phase::Execute {
            for (family, &count) in &ctx.tool_failures_by_family {
                // 第二次同类 drift 才附加（drift_reported 在 note_tool_failure
                // 首次跨越阈值时 +1：第一次=1 → Warning 由 drift_detection 门
                // 产出；第二次=2 → 本处附加「需人工确认」Warning）。
                if count >= DRIFT_THRESHOLD
                    && self.drift_reported.get(family).copied().unwrap_or(0) >= 2
                {
                    violations.push(GateViolation {
                        gate: Cow::Borrowed("drift-detection"),
                        phase,
                        severity: FindingSeverity::Warning,
                        detail: format!(
                            "tool family '{family}' drifted again ({count} consecutive \
                             failures) — 需人工确认是否换策略"
                        ),
                    });
                }
            }
        }
        self.violations += violations.len() as u32;
        violations
    }

    /// 记录一次工具调用结果（滑动窗口）：成功清零该族连续失败；失败递增，
    /// **首次**达到 `DRIFT_THRESHOLD`（count == 阈值）时返回 `DriftFinding`
    /// 并计入二次判定（Bugbot #5：阈值跨越后、窗口未清零前继续失败不重复
    /// 发事件、不重复 +1）。每次调用都计入 `tool_calls`。
    ///
    /// Bugbot #4：drift 未启用（门被摘除/未注册）时只计 `tool_calls`，
    /// 不维护失败窗口、不产出 DriftFinding（agent.rs 侧发事件与二次附加
    /// 一并失效）。
    pub fn note_tool_failure(&mut self, family: &str, succeeded: bool) -> Option<DriftFinding> {
        self.tool_calls += 1;
        if !self.drift_enabled {
            return None;
        }
        if succeeded {
            self.fail_streak.insert(family.to_string(), 0);
            return None;
        }
        let count = self.fail_streak.entry(family.to_string()).or_insert(0);
        *count += 1;
        if *count == DRIFT_THRESHOLD {
            *self.drift_reported.entry(family.to_string()).or_insert(0) += 1;
            Some(DriftFinding {
                tool_family: family.to_string(),
                failures: *count,
                detail: format!("tool family '{family}' failed {count} times consecutively"),
            })
        } else {
            None
        }
    }

    /// 记录一次 Verification 事件（与 `metrics.observe_verify` 同处调用）。
    pub fn observe_verify(&mut self, passed: bool) {
        self.verify_events += 1;
        if passed {
            self.verify_passed += 1;
        } else {
            self.verify_failed += 1;
        }
    }

    /// 会话内已产出计划性文本（单调置位；供 plan-before-execute 门读取）。
    pub fn set_has_plan_text(&mut self, v: bool) {
        if v {
            self.has_plan_text = true;
        }
    }

    /// Distill 阶段是否已产出 lesson（会话收尾前由调用方设置）。
    pub fn set_has_lesson(&mut self, v: bool) {
        self.has_lesson = v;
    }

    /// verify-evidence 证据链判定：会话内是否有任一 passed 的 Verification 事件。
    pub fn verify_evidence_passed(&self) -> bool {
        self.verify_passed >= 1
    }

    /// Verification 事件总数（供调用方判断"零事件"降级路径）。
    pub fn verify_event_count(&self) -> u32 {
        self.verify_events
    }

    /// 统计快照 `(violations, transitions)`，供 QualitySummary 消费。
    pub fn stats(&self) -> (u32, u32) {
        (self.violations, self.transitions)
    }

    /// 构造门控上下文（verify 计数/滑动窗口/计划文本等来自本运行器状态）。
    pub fn build_ctx(
        &self,
        phase: Phase,
        verify_configured: bool,
        findings: Vec<QualityFinding>,
    ) -> PhaseGateCtx {
        PhaseGateCtx {
            phase,
            verify_configured,
            verify_passed_count: self.verify_passed,
            verify_failed_count: self.verify_failed,
            tool_calls: self.tool_calls,
            tool_failures_by_family: self.fail_streak.clone(),
            findings,
            has_lesson: self.has_lesson,
            has_plan_text: self.has_plan_text,
            run_ms: self.run_started.elapsed().as_millis() as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with(
        phase: Phase,
        verify_configured: bool,
        passed: u32,
        failed: u32,
        tool_calls: u32,
        failures: &[(&str, u32)],
        has_lesson: bool,
        has_plan_text: bool,
    ) -> PhaseGateCtx {
        PhaseGateCtx {
            phase,
            verify_configured,
            verify_passed_count: passed,
            verify_failed_count: failed,
            tool_calls,
            tool_failures_by_family: failures.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            findings: Vec::new(),
            has_lesson,
            has_plan_text,
            run_ms: 0,
        }
    }

    fn gates() -> Vec<Arc<dyn PhaseGate>> {
        builtin_phase_gates(&HashMap::new())
    }

    #[test]
    fn gate_level_from_str_accepts_three_values() {
        assert_eq!("hard".parse::<GateLevel>().unwrap(), GateLevel::Hard);
        assert_eq!("soft".parse::<GateLevel>().unwrap(), GateLevel::Soft);
        assert_eq!("off".parse::<GateLevel>().unwrap(), GateLevel::Off);
        assert!("HARD".parse::<GateLevel>().is_err());
        assert!("".parse::<GateLevel>().is_err());
        assert!("banana".parse::<GateLevel>().is_err());
    }

    #[test]
    fn plan_before_execute_warns_on_no_plan_text() {
        let g = gates();
        let ctx = ctx_with(
            Phase::Execute,
            false,
            0,
            0,
            0,
            &[],
            false,
            false, // 无计划文本
        );
        let vs: Vec<_> = g.iter().flat_map(|x| x.check(&ctx)).collect();
        assert!(
            vs.iter()
                .any(|v| v.gate == "plan-before-execute" && v.severity == FindingSeverity::Warning),
            "{vs:?}"
        );

        // 有计划文本 → 通过
        let ctx = ctx_with(Phase::Execute, false, 0, 0, 0, &[], false, true);
        let vs: Vec<_> = g.iter().flat_map(|x| x.check(&ctx)).collect();
        assert!(!vs.iter().any(|v| v.gate == "plan-before-execute"));

        // 非 Execute 阶段不评估
        let ctx = ctx_with(Phase::Plan, false, 0, 0, 0, &[], false, false);
        let vs: Vec<_> = g.iter().flat_map(|x| x.check(&ctx)).collect();
        assert!(!vs.iter().any(|v| v.gate == "plan-before-execute"));
    }

    #[test]
    fn verify_evidence_gate_branches() {
        let g = gates();
        // 未配置 → 通过
        let ctx = ctx_with(Phase::Distill, false, 0, 0, 0, &[], false, false);
        let vs: Vec<_> = g.iter().flat_map(|x| x.check(&ctx)).collect();
        assert!(!vs.iter().any(|v| v.gate == "verify-evidence"));

        // 配置 + passed>=1 → 通过
        let ctx = ctx_with(Phase::Distill, true, 1, 0, 0, &[], false, false);
        let vs: Vec<_> = g.iter().flat_map(|x| x.check(&ctx)).collect();
        assert!(!vs.iter().any(|v| v.gate == "verify-evidence"));

        // 配置 + 零 passed + 有失败 → Blocking
        let ctx = ctx_with(Phase::Distill, true, 0, 2, 0, &[], false, false);
        let vs: Vec<_> = g.iter().flat_map(|x| x.check(&ctx)).collect();
        assert!(
            vs.iter()
                .any(|v| v.gate == "verify-evidence" && v.severity == FindingSeverity::Blocking),
            "{vs:?}"
        );

        // 配置 + 零事件 → Info（bash 缺失降级）
        let ctx = ctx_with(Phase::Distill, true, 0, 0, 0, &[], false, false);
        let vs: Vec<_> = g.iter().flat_map(|x| x.check(&ctx)).collect();
        assert!(
            vs.iter()
                .any(|v| v.gate == "verify-evidence" && v.severity == FindingSeverity::Info),
            "{vs:?}"
        );

        // 非 Verify/Distill 阶段不评估（避免 Execute 阶段误报）
        let ctx = ctx_with(Phase::Execute, true, 0, 2, 0, &[], false, false);
        let vs: Vec<_> = g.iter().flat_map(|x| x.check(&ctx)).collect();
        assert!(!vs.iter().any(|v| v.gate == "verify-evidence"));
    }

    #[test]
    fn distill_on_complex_gate_branches() {
        let g = gates();
        // >20 工具调用且无 lesson → Warning
        let ctx = ctx_with(Phase::Distill, false, 0, 0, 21, &[], false, false);
        let vs: Vec<_> = g.iter().flat_map(|x| x.check(&ctx)).collect();
        assert!(
            vs.iter()
                .any(|v| v.gate == "distill-on-complex" && v.severity == FindingSeverity::Warning),
            "{vs:?}"
        );

        // 已产出 lesson → 通过
        let ctx = ctx_with(Phase::Distill, false, 0, 0, 21, &[], true, false);
        let vs: Vec<_> = g.iter().flat_map(|x| x.check(&ctx)).collect();
        assert!(!vs.iter().any(|v| v.gate == "distill-on-complex"));

        // 工具调用少 → 通过
        let ctx = ctx_with(Phase::Distill, false, 0, 0, 5, &[], false, false);
        let vs: Vec<_> = g.iter().flat_map(|x| x.check(&ctx)).collect();
        assert!(!vs.iter().any(|v| v.gate == "distill-on-complex"));
    }

    #[test]
    fn drift_detection_gate_warns_on_three_consecutive_failures() {
        let g = gates();
        let ctx = ctx_with(Phase::Execute, false, 0, 0, 0, &[("bash", 3)], false, false);
        let vs: Vec<_> = g.iter().flat_map(|x| x.check(&ctx)).collect();
        assert!(
            vs.iter()
                .any(|v| v.gate == "drift-detection" && v.severity == FindingSeverity::Warning),
            "{vs:?}"
        );

        // 未达阈值 → 通过
        let ctx = ctx_with(Phase::Execute, false, 0, 0, 0, &[("bash", 2)], false, false);
        let vs: Vec<_> = g.iter().flat_map(|x| x.check(&ctx)).collect();
        assert!(!vs.iter().any(|v| v.gate == "drift-detection"));
    }

    #[test]
    fn hard_level_escalates_warning_to_blocking() {
        let levels = HashMap::from([("plan-before-execute".to_string(), GateLevel::Hard)]);
        let g = builtin_phase_gates(&levels);
        let ctx = ctx_with(Phase::Execute, false, 0, 0, 0, &[], false, false);
        let vs: Vec<_> = g.iter().flat_map(|x| x.check(&ctx)).collect();
        assert!(
            vs.iter().any(|v| v.gate == "plan-before-execute"
                && v.severity == FindingSeverity::Blocking),
            "{vs:?}"
        );
    }

    #[test]
    fn off_level_suppresses_gate() {
        let levels = HashMap::from([("drift-detection".to_string(), GateLevel::Off)]);
        let g = builtin_phase_gates(&levels);
        let ctx = ctx_with(Phase::Execute, false, 0, 0, 0, &[("bash", 5)], false, false);
        let vs: Vec<_> = g.iter().flat_map(|x| x.check(&ctx)).collect();
        assert!(!vs.iter().any(|v| v.gate == "drift-detection"));
    }

    #[test]
    fn sliding_window_counts_consecutive_failures_and_resets_on_success() {
        let mut r = PhaseRunner::new();
        assert!(r.note_tool_failure("bash", false).is_none());
        assert!(r.note_tool_failure("bash", false).is_none());
        let drift = r
            .note_tool_failure("bash", false)
            .expect("3rd failure → drift");
        assert_eq!(drift.tool_family, "bash");
        assert_eq!(drift.failures, 3);

        // 成功清零
        assert!(r.note_tool_failure("bash", true).is_none());
        assert!(r.note_tool_failure("bash", false).is_none());
        assert!(r.note_tool_failure("bash", false).is_none());
        // 清零后再次第 3 次失败 → 第二次 drift
        let second = r.note_tool_failure("bash", false).expect("second drift");
        assert_eq!(second.failures, 3);
    }

    #[test]
    fn second_drift_of_same_family_escalates_to_warning_needs_human_confirm() {
        // Bugbot #3 修正语义：二次 drift 不再 Blocking（无阶段级 Ask 桥），
        // 降级为 Warning + 「需人工确认」标注，且只在 Execute transition 附加。
        let g = gates();
        let mut r = PhaseRunner::new();

        // 第一次 drift：连续 3 次失败
        for _ in 0..3 {
            r.note_tool_failure("bash", false);
        }
        let ctx = r.build_ctx(Phase::Execute, false, Vec::new());
        let vs = r.transition(Phase::Execute, &g, &ctx);
        assert!(vs.iter().any(|v| v.gate == "drift-detection"));
        assert!(!vs.iter().any(|v| v.severity == FindingSeverity::Blocking));

        // 成功清零后再次连续 3 次失败（第二次 drift）→ Warning + 需人工确认
        r.note_tool_failure("bash", true);
        for _ in 0..3 {
            r.note_tool_failure("bash", false);
        }
        let ctx = r.build_ctx(Phase::Execute, false, Vec::new());
        let vs = r.transition(Phase::Execute, &g, &ctx);
        let second: Vec<_> = vs.iter().filter(|v| v.gate == "drift-detection").collect();
        assert!(!second.is_empty(), "{vs:?}");
        assert!(
            second.iter().any(|v| v.detail.contains("需人工确认")),
            "{vs:?}"
        );
        assert!(
            !vs.iter().any(|v| v.severity == FindingSeverity::Blocking),
            "second drift must not Blocking (Ask 桥为工具级，drift 为阶段级)：{vs:?}"
        );
    }

    #[test]
    fn drift_emits_finding_only_on_first_threshold_crossing() {
        // Bugbot #5：阈值跨越后、窗口未清零前继续失败不重复发事件、不重复计数。
        let mut r = PhaseRunner::new();
        assert!(r.note_tool_failure("bash", false).is_none());
        assert!(r.note_tool_failure("bash", false).is_none());
        let first = r
            .note_tool_failure("bash", false)
            .expect("3rd failure → drift");
        assert_eq!(first.failures, 3);
        // 第 4、5 次失败（仍在窗口内、无成功）→ 不再发事件。
        assert!(r.note_tool_failure("bash", false).is_none());
        assert!(r.note_tool_failure("bash", false).is_none());
        // drift_reported 只 +1 一次 → 此时 Execute transition 不附加二次 Warning。
        let g = gates();
        let ctx = r.build_ctx(Phase::Execute, false, Vec::new());
        let vs = r.transition(Phase::Execute, &g, &ctx);
        assert_eq!(
            vs.iter()
                .filter(|v| v.gate == "drift-detection" && v.detail.contains("需人工确认"))
                .count(),
            0,
            "{vs:?}"
        );
        // 成功清零后再次跨越阈值 → 第二次 drift（事件 + 计数各一次）。
        r.note_tool_failure("bash", true);
        for _ in 0..3 {
            r.note_tool_failure("bash", false);
        }
        let second = r.note_tool_failure("bash", false);
        assert!(second.is_none(), "第 4 次失败不重复发事件");
        let ctx = r.build_ctx(Phase::Execute, false, Vec::new());
        let vs = r.transition(Phase::Execute, &g, &ctx);
        assert!(
            vs.iter()
                .any(|v| v.gate == "drift-detection" && v.detail.contains("需人工确认")),
            "window reset 后再次 drift → 附加二次 Warning：{vs:?}"
        );
    }

    #[test]
    fn secondary_drift_warning_only_attached_at_execute_transition() {
        // Bugbot #5：二次附加只在 Execute transition 做，Plan 等其他 phase 不附加。
        let g = gates();
        let mut r = PhaseRunner::new();
        for _ in 0..3 {
            r.note_tool_failure("bash", false);
        }
        r.note_tool_failure("bash", true);
        for _ in 0..3 {
            r.note_tool_failure("bash", false);
        }
        // Plan transition（同轮 Plan/Execute 双计场景）→ 无二次附加。
        let ctx = r.build_ctx(Phase::Plan, false, Vec::new());
        let vs = r.transition(Phase::Plan, &g, &ctx);
        assert!(
            !vs.iter()
                .any(|v| v.gate == "drift-detection" && v.detail.contains("需人工确认")),
            "Plan transition 不得附加二次 Warning：{vs:?}"
        );
        // Execute transition → 附加。
        let ctx = r.build_ctx(Phase::Execute, false, Vec::new());
        let vs = r.transition(Phase::Execute, &g, &ctx);
        assert!(
            vs.iter()
                .any(|v| v.gate == "drift-detection" && v.detail.contains("需人工确认")),
            "Execute transition 应附加二次 Warning：{vs:?}"
        );
    }

    #[test]
    fn drift_off_disables_counting_finding_and_secondary() {
        // Bugbot #4：drift-detection=off → 4 连败不产 DriftFinding、不产
        // drift 违规、不附加二次处理。门从列表摘除，PhaseRunner 探测关闭计数。
        let levels = HashMap::from([("drift-detection".to_string(), GateLevel::Off)]);
        let g = builtin_phase_gates(&levels);
        assert!(
            !g.iter().any(|x| x.name() == "drift-detection"),
            "off 时 drift 门必须从列表摘除（PhaseRunner 靠存在性探测开关）"
        );
        let mut r = PhaseRunner::new();
        // 先 transition 一次让 runner 探测到 drift 门缺失（模拟主循环顺序）。
        let ctx = r.build_ctx(Phase::Execute, false, Vec::new());
        let _ = r.transition(Phase::Execute, &g, &ctx);
        for _ in 0..4 {
            assert!(
                r.note_tool_failure("bash", false).is_none(),
                "drift off 时不得产 DriftFinding"
            );
        }
        let ctx = r.build_ctx(Phase::Execute, false, Vec::new());
        let vs = r.transition(Phase::Execute, &g, &ctx);
        assert!(
            !vs.iter().any(|v| v.gate == "drift-detection"),
            "drift off 时不得产 drift 违规：{vs:?}"
        );
        assert!(
            !vs.iter().any(|v| v.severity == FindingSeverity::Blocking),
            "{vs:?}"
        );
        // 计数被关闭：tool_calls 仍计入（distill-on-complex 用），窗口不涨。
        let (_, _) = r.stats();
        let ctx = r.build_ctx(Phase::Execute, false, Vec::new());
        assert!(
            ctx.tool_calls == 4,
            "tool_calls 仍应计数（供 distill 门）：{}",
            ctx.tool_calls
        );
    }

    #[test]
    fn stats_counts_violations_and_transitions() {
        let g = gates();
        let mut r = PhaseRunner::new();
        let ctx = r.build_ctx(Phase::Understand, false, Vec::new());
        assert!(r.transition(Phase::Understand, &g, &ctx).is_empty());
        let ctx = r.build_ctx(Phase::Execute, false, Vec::new());
        let vs = r.transition(Phase::Execute, &g, &ctx); // plan-before-execute → 1
        assert!(!vs.is_empty());
        let (v, t) = r.stats();
        assert_eq!(v, 1);
        assert_eq!(t, 2);
    }

    #[test]
    fn verify_evidence_passed_reflects_passed_events() {
        let mut r = PhaseRunner::new();
        assert!(!r.verify_evidence_passed());
        r.observe_verify(false);
        assert!(!r.verify_evidence_passed());
        r.observe_verify(true);
        assert!(r.verify_evidence_passed());
        assert_eq!(r.verify_event_count(), 2);
    }

    #[test]
    fn has_plan_text_is_monotonic() {
        let mut r = PhaseRunner::new();
        r.set_has_plan_text(false);
        r.set_has_plan_text(true);
        r.set_has_plan_text(false); // 不回落
        let ctx = r.build_ctx(Phase::Execute, false, Vec::new());
        assert!(ctx.has_plan_text);
    }
}
