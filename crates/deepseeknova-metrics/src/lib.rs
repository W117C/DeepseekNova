//! # deepseeknova-metrics
//!
//! 会话级效能度量：每个 `run_stream` 内用 [`SessionTracker`] 局部累加
//! 执行面指标（步数/工具成败/重试/验证/outcome），run 结束时快照为
//! [`SessionStats`]，由 runtime 组装 [`SessionReport`]（叠加成本面）落盘。
//!
//! Tracker 刻意保持“局部变量 + 同步累加”语义：Agent 是共享实例，并发 run
//! 各自持有独立 tracker，互不污染，无需 Mutex 或 run_id 隔离。

use deepseeknova_core::tool_hook::{FindingSeverity, QualityFinding};
use deepseeknova_provider::cost::CostReport;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// run 结束态。未覆盖的优雅暂停（budget/verify/review）保留 `None`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunOutcome {
    /// 正常完成（工具调用后无进一步调用）。
    Completed,
    /// max_steps 到顶（优雅暂停）。
    PausedMaxSteps,
    /// 取消令牌触发。
    Cancelled,
}

/// 一次 run 的执行面快照。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionStats {
    /// tracker::new() 时的系统时间戳（毫秒）。
    pub started_at_ms: u64,
    /// snapshot() 时计算（now - started_at）。
    pub duration_ms: u64,
    pub steps: u64,
    pub tool_calls: u64,
    pub tool_failures: u64,
    pub tool_failures_by_name: HashMap<String, u64>,
    pub tool_calls_by_name: HashMap<String, u64>,
    pub retries: u64,
    pub verifications: u64,
    pub verifications_passed: u64,
    /// run 结束前为 None。
    pub outcome: Option<RunOutcome>,
}

/// 快照别名（hook 参数类型，设计文档中的 SessionSnapshot）。
pub type SessionSnapshot = SessionStats;

/// 落盘报告：执行面 + 成本面。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionReport {
    pub session_id: String,
    pub stats: SessionStats,
    /// 成本为累计语义（CostLedger 为 Agent 级共享、跨 run 累计）。
    pub cost: CostReport,
}

/// run 内的局部累加器（同步、非共享）。每个 run 实例化一个，run 结束
/// snapshot 后经 hook 传出；并发 run 同一 Agent 实例互不污染。
#[derive(Debug, Clone, Default)]
pub struct SessionTracker {
    started_at: Option<Instant>,
    started_at_ms: u64,
    steps: u64,
    tool_calls: u64,
    tool_failures: u64,
    tool_failures_by_name: HashMap<String, u64>,
    tool_calls_by_name: HashMap<String, u64>,
    retries: u64,
    verifications: u64,
    verifications_passed: u64,
    outcome: Option<RunOutcome>,
}

impl SessionTracker {
    pub fn new() -> Self {
        Self {
            started_at: Some(Instant::now()),
            started_at_ms: now_millis(),
            ..Self::default()
        }
    }

    pub fn observe_step(&mut self) {
        self.steps += 1;
    }

    /// 记录一次工具调用结果。`ok=false` 时同时计入失败次数与按名失败表。
    pub fn observe_tool_call(&mut self, name: &str, ok: bool) {
        self.tool_calls += 1;
        *self.tool_calls_by_name.entry(name.to_string()).or_default() += 1;
        if !ok {
            self.tool_failures += 1;
            *self
                .tool_failures_by_name
                .entry(name.to_string())
                .or_default() += 1;
        }
    }

    pub fn observe_retry(&mut self) {
        self.retries += 1;
    }

    pub fn observe_verify(&mut self, passed: bool) {
        self.verifications += 1;
        if passed {
            self.verifications_passed += 1;
        }
    }

    pub fn mark_outcome(&mut self, outcome: RunOutcome) {
        self.outcome = Some(outcome);
    }

    pub fn snapshot(&self) -> SessionStats {
        SessionStats {
            started_at_ms: self.started_at_ms,
            duration_ms: self
                .started_at
                .map(|t| t.elapsed().as_millis() as u64)
                .unwrap_or(0),
            steps: self.steps,
            tool_calls: self.tool_calls,
            tool_failures: self.tool_failures,
            tool_failures_by_name: self.tool_failures_by_name.clone(),
            tool_calls_by_name: self.tool_calls_by_name.clone(),
            retries: self.retries,
            verifications: self.verifications,
            verifications_passed: self.verifications_passed,
            outcome: self.outcome,
        }
    }
}

/// 生成会话报告 ID：`<epoch毫秒>-<pid>-<进程内序号>`，并发进程/线程天然隔离。
pub fn new_session_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}-{}",
        now_millis(),
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// 将报告写为 `<session_id>.json`（目录不存在则创建）。失败只由调用方
/// warn，不阻断 run。
pub fn write_report(report: &SessionReport, dir: &Path) -> std::io::Result<std::path::PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.json", report.session_id));
    let bytes = serde_json::to_vec_pretty(report)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, bytes)?;
    Ok(path)
}

// ---------------------------------------------------------------------------
// 评分卡（任务质量闭环 C：跨会话聚合的质量分数；设计 §7.1 起含协议维与综合指数）
// ---------------------------------------------------------------------------

/// 评分卡的维度分数（各维 0.0..=1.0，越接近 1.0 越好）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScoreDimensions {
    /// 守规：1 - blocking 级违规 finding 数 / max(tool_calls, 1)，clamp 0..=1。
    pub governance: f32,
    /// 验证：verifications_passed / max(verifications, 1)。
    pub verification: f32,
    /// 反思：失败路径中有 reflection 记录的比例。
    pub reflection: f32,
    /// 审查：审查轮中判 Approve 的占比；空审查（无数据）按 1.0。
    pub review: f32,
    /// 协议：1 - gate 违规数 / 阶段迁移数（[`protocol_dim`]）；无协议数据按 1.0。
    /// serde default 1.0（`protocol_default` 私有函数）：旧评分卡文件缺该字段时反序列化
    /// 为 1.0，与 [`Scorecard::compute`] 无协议输入口径（1.0）一致，
    /// 混合新旧卡聚合时不系统性低估（Finding #6）。
    #[serde(default = "protocol_default")]
    pub protocol: f32,
    /// 综合指数：五维加权均值（[`composite_index`]，权重见 [`COMPOSITE_WEIGHTS`]）。
    /// serde default 1.0（`protocol_default` 私有函数）：旧评分卡文件缺该字段时按 1.0
    /// 反序列化（serde 不重算 composite_index），与 protocol 维默认口径同为
    /// "无数据按满分"，聚合不低估；compute() 路径仍会重算为真实加权均值。
    #[serde(default = "protocol_default")]
    pub composite: f32,
}

/// [`ScoreDimensions::protocol`] / [`ScoreDimensions::composite`] 的 serde
/// 反序列化默认值：与 [`protocol_dim`] 的 0/0 口径（无数据按 1.0）一致。
fn protocol_default() -> f32 {
    1.0
}

/// 协议维公式：`transitions == 0` 时按 1.0（无迁移即无违规机会）；
/// 否则 `1 - violations / transitions`，clamp 到 [0.0, 1.0]
/// （违规数超过迁移数时钳到 0.0）。
pub fn protocol_dim(violations: u32, transitions: u32) -> f32 {
    if transitions == 0 {
        1.0
    } else {
        (1.0 - violations as f32 / transitions as f32).clamp(0.0, 1.0)
    }
}

fn governance_of(d: &ScoreDimensions) -> f32 {
    d.governance
}
fn verification_of(d: &ScoreDimensions) -> f32 {
    d.verification
}
fn protocol_of(d: &ScoreDimensions) -> f32 {
    d.protocol
}
fn reflection_of(d: &ScoreDimensions) -> f32 {
    d.reflection
}
fn review_of(d: &ScoreDimensions) -> f32 {
    d.review
}

/// 综合指数权重项：维度取值函数 + 权重。
pub type WeightedDim = (fn(&ScoreDimensions) -> f32, f32);

/// 综合指数权重（设计 §7.1：governance 0.30 / verification 0.25 /
/// protocol 0.20 / reflection 0.15 / review 0.10，合计 1.00）。
pub const COMPOSITE_WEIGHTS: [WeightedDim; 5] = [
    (governance_of, 0.30),
    (verification_of, 0.25),
    (protocol_of, 0.20),
    (reflection_of, 0.15),
    (review_of, 0.10),
];

/// 综合指数：`Σ(w_i * dim_i)`（五维加权均值，各维均落在 0.0..=1.0 时结果亦在区间内）。
pub fn composite_index(dims: &ScoreDimensions) -> f32 {
    COMPOSITE_WEIGHTS.iter().map(|(dim, w)| dim(dims) * w).sum()
}

/// 单会话四维评分卡（独立于 [`SessionReport`] 落盘，不破坏既有格式）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scorecard {
    pub session_id: String,
    pub started_at_ms: u64,
    pub dimensions: ScoreDimensions,
}

impl Scorecard {
    /// 四维均值（governance/verification/reflection/review，不含 protocol/composite；
    /// 综合指数见 [`composite_index`]，本方法保持既有语义）。
    pub fn overall(&self) -> f32 {
        (self.dimensions.governance
            + self.dimensions.verification
            + self.dimensions.reflection
            + self.dimensions.review)
            / 4.0
    }

    /// 由执行面快照与质量摘要计算评分卡。
    ///
    /// 公式（设计 §5.2 的实现口径）：
    /// - governance：1 - blocking 级违规数 / max(tool_calls, 1)，clamp 0..=1。
    ///   仅统计 `passed=false` 的 Blocking finding（`passed=true` 属审计通过记录）；
    ///   无违规即 1.0。
    /// - verification：verifications_passed / max(verifications, 1)。
    /// - reflection：min(reflection_count / max(tool_failures + verify_failures, 1), 1.0)，
    ///   verify_failures = verifications - verifications_passed。
    /// - review：review_passes / max(review_passes + review_issues, 1)；
    ///   空审查（两者皆 0）按 1.0 —— 宽松口径：无审查即无问题。
    ///
    /// protocol/composite：本方法无协议统计输入，protocol 维按 1.0（无迁移即无违规，
    /// [`protocol_dim`] 的 0/0 口径），composite 由 [`composite_index`] 计算。
    /// 协议增强的调用方（runtime worker）应另行用 [`protocol_dim`] + [`composite_index`]
    /// 计算后覆写 `dimensions.protocol` / `dimensions.composite`（字段 pub）。
    pub fn compute(
        session_id: &str,
        stats: &SessionStats,
        quality: &[QualityFinding],
        reflection_count: u32,
        review_issues: u32,
        review_passes: u32,
    ) -> Scorecard {
        let blocking = quality
            .iter()
            .filter(|f| f.severity == FindingSeverity::Blocking && !f.passed)
            .count() as f32;
        let governance = (1.0 - blocking / stats.tool_calls.max(1) as f32).clamp(0.0, 1.0);
        let verification = stats.verifications_passed as f32 / stats.verifications.max(1) as f32;
        let verify_failures = stats
            .verifications
            .saturating_sub(stats.verifications_passed);
        let failure_paths = stats.tool_failures + verify_failures;
        let reflection = (reflection_count as f32 / failure_paths.max(1) as f32).min(1.0);
        let review = if review_passes + review_issues == 0 {
            1.0
        } else {
            review_passes as f32 / (review_passes + review_issues) as f32
        };
        let mut dims = ScoreDimensions {
            governance,
            verification,
            reflection,
            review,
            protocol: 1.0,
            composite: 0.0,
        };
        dims.composite = composite_index(&dims);
        Scorecard {
            session_id: session_id.to_string(),
            started_at_ms: stats.started_at_ms,
            dimensions: dims,
        }
    }
}

impl Scorecard {
    /// 由已计算的四维与协议统计填充分数卡维度：
    /// 设置 `protocol` 维并据此重算 `composite`（调用方在 [`Scorecard::compute`]
    /// 之后调用，或用于独立构造）。
    pub fn fill_protocol(&mut self, violations: u32, transitions: u32) {
        self.dimensions.protocol = protocol_dim(violations, transitions);
        self.dimensions.composite = composite_index(&self.dimensions);
    }
}

/// 跨会话聚合结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScorecardAggregate {
    pub count: usize,
    pub avg: ScoreDimensions,
    pub avg_overall: f32,
    /// 平均分最低的维度名（governance/verification/reflection/review）；
    /// 空输入为 `""`。
    pub worst_dimension: String,
}

/// 将评分卡写为 `<dir>/<session_id>.scorecard.json`（目录不存在则创建）。
/// 失败只由调用方 warn，不阻断 run。独立文件，不影响 [`SessionReport`]。
pub fn write_scorecard(card: &Scorecard, dir: &Path) -> std::io::Result<std::path::PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.scorecard.json", card.session_id));
    let bytes = serde_json::to_vec_pretty(card)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, bytes)?;
    Ok(path)
}

/// 扫描目录下全部 `*.scorecard.json` 并解析；解析失败或非评分卡文件跳过。
pub fn list_scorecards(dir: &Path) -> Vec<Scorecard> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut cards = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_scorecard = path
            .file_name()
            .map(|n| n.to_string_lossy().ends_with(".scorecard.json"))
            .unwrap_or(false);
        if !is_scorecard {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(card) = serde_json::from_str::<Scorecard>(&text) {
                cards.push(card);
            }
        }
    }
    cards
}

/// 聚合多张评分卡：各维均值、overall 均值与平均分最低的维度。
/// 空输入返回全零聚合、`worst_dimension = ""`。
pub fn aggregate_scorecards(cards: &[Scorecard]) -> ScorecardAggregate {
    let count = cards.len();
    if count == 0 {
        return ScorecardAggregate {
            count: 0,
            avg: ScoreDimensions {
                governance: 0.0,
                verification: 0.0,
                reflection: 0.0,
                review: 0.0,
                protocol: 0.0,
                composite: 0.0,
            },
            avg_overall: 0.0,
            worst_dimension: String::new(),
        };
    }
    let mut sum = ScoreDimensions {
        governance: 0.0,
        verification: 0.0,
        reflection: 0.0,
        review: 0.0,
        protocol: 0.0,
        composite: 0.0,
    };
    for c in cards {
        sum.governance += c.dimensions.governance;
        sum.verification += c.dimensions.verification;
        sum.reflection += c.dimensions.reflection;
        sum.review += c.dimensions.review;
        sum.protocol += c.dimensions.protocol;
        sum.composite += c.dimensions.composite;
    }
    let n = count as f32;
    let avg = ScoreDimensions {
        governance: sum.governance / n,
        verification: sum.verification / n,
        reflection: sum.reflection / n,
        review: sum.review / n,
        protocol: sum.protocol / n,
        composite: sum.composite / n,
    };
    let avg_overall = (avg.governance + avg.verification + avg.reflection + avg.review) / 4.0;
    // 固定顺序扫描取首个最小值，保证同名维度结果稳定。
    let mut worst = "governance";
    let mut worst_value = avg.governance;
    for (name, value) in [
        ("verification", avg.verification),
        ("reflection", avg.reflection),
        ("review", avg.review),
    ] {
        if value < worst_value {
            worst = name;
            worst_value = value;
        }
    }
    ScorecardAggregate {
        count,
        avg,
        avg_overall,
        worst_dimension: worst.to_string(),
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_provider::cost::{CostReport, CostRow, ModelRole, UsageBucket};

    #[test]
    fn empty_tracker_snapshot_is_zero() {
        let stats = SessionTracker::new().snapshot();
        assert_eq!(stats.steps, 0);
        assert_eq!(stats.tool_calls, 0);
        assert_eq!(stats.tool_failures, 0);
        assert_eq!(stats.retries, 0);
        assert_eq!(stats.verifications, 0);
        assert_eq!(stats.outcome, None);
        assert!(stats.duration_ms < 1000);
        assert!(stats.started_at_ms > 0);
    }

    #[test]
    fn tracker_accumulates_all_dimensions() {
        let mut t = SessionTracker::new();
        t.observe_step();
        t.observe_step();
        t.observe_tool_call("read_file", true);
        t.observe_tool_call("bash", false);
        t.observe_retry();
        t.observe_verify(true);
        t.observe_verify(false);
        t.mark_outcome(RunOutcome::Completed);
        let s = t.snapshot();
        assert_eq!(s.steps, 2);
        assert_eq!(s.tool_calls, 2);
        assert_eq!(s.tool_failures, 1);
        assert_eq!(s.tool_calls_by_name.get("read_file"), Some(&1));
        assert_eq!(s.tool_failures_by_name.get("bash"), Some(&1));
        assert_eq!(s.retries, 1);
        assert_eq!(s.verifications, 2);
        assert_eq!(s.verifications_passed, 1);
        assert_eq!(s.outcome, Some(RunOutcome::Completed));
    }

    #[test]
    fn report_serializes_roundtrip() {
        let mut t = SessionTracker::new();
        t.observe_step();
        t.mark_outcome(RunOutcome::Completed);
        let report = SessionReport {
            session_id: new_session_id(),
            stats: t.snapshot(),
            cost: CostReport {
                rows: vec![CostRow {
                    model: "deepseek-v4-pro".into(),
                    role: ModelRole::Main,
                    bucket: UsageBucket {
                        prompt_tokens: 10,
                        completion_tokens: 20,
                        ..Default::default()
                    },
                    cost_usd: Some(0.001),
                }],
                total_usd: Some(0.001),
                unmetered_calls: 0,
            },
        };
        let json = serde_json::to_string(&report).unwrap();
        let back: SessionReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back.stats.steps, 1);
        assert_eq!(back.stats.outcome, Some(RunOutcome::Completed));
        assert_eq!(back.cost.total_usd, Some(0.001));
    }

    #[test]
    fn write_report_creates_file_in_temp_dir() {
        let dir = tempfile::tempdir().unwrap();
        let report = SessionReport {
            session_id: "t-1".into(),
            stats: SessionTracker::new().snapshot(),
            cost: CostReport::default(),
        };
        let path = write_report(&report, dir.path()).unwrap();
        assert!(path.exists());
        let back: SessionReport =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back.session_id, "t-1");
    }

    fn blocking(rule: &str, passed: bool) -> QualityFinding {
        QualityFinding {
            rule: rule.to_string(),
            severity: FindingSeverity::Blocking,
            passed,
            evidence: "e".to_string(),
        }
    }

    #[test]
    fn scorecard_no_violations_and_empty_review_are_full_marks() {
        // 0 finding → governance 1.0；全部验证通过 → verification 1.0；
        // 空审查（无 review 数据）→ review 1.0（宽松口径）；无失败 → reflection 0.0。
        let stats = SessionStats {
            tool_calls: 5,
            verifications: 2,
            verifications_passed: 2,
            ..Default::default()
        };
        let card = Scorecard::compute("s1", &stats, &[], 0, 0, 0);
        assert_eq!(card.dimensions.governance, 1.0);
        assert_eq!(card.dimensions.verification, 1.0);
        assert_eq!(card.dimensions.reflection, 0.0);
        assert_eq!(card.dimensions.review, 1.0);
        assert_eq!(card.started_at_ms, stats.started_at_ms);
    }

    #[test]
    fn scorecard_all_failures_drive_dims_to_zero() {
        // 全部工具失败 + 全部验证失败 + 无反思 + 审查有问题 → 各维 0.0。
        let stats = SessionStats {
            tool_calls: 3,
            tool_failures: 3,
            verifications: 2,
            verifications_passed: 0,
            ..Default::default()
        };
        let f = blocking("no-commit-secret", false);
        let card = Scorecard::compute("s2", &stats, &[f.clone(), f.clone(), f.clone()], 0, 1, 0);
        assert_eq!(card.dimensions.governance, 0.0);
        assert_eq!(card.dimensions.verification, 0.0);
        assert_eq!(card.dimensions.reflection, 0.0);
        assert_eq!(card.dimensions.review, 0.0);
        assert_eq!(card.overall(), 0.0);
    }

    #[test]
    fn scorecard_mixed_dims_and_overall_mean() {
        let stats = SessionStats {
            tool_calls: 10,
            tool_failures: 2,
            verifications: 4,
            verifications_passed: 3,
            ..Default::default()
        };
        let card = Scorecard::compute(
            "s3",
            &stats,
            &[blocking("no-commit-secret", false)],
            2, // reflection：2 / (2 工具失败 + 1 验证失败) = 2/3
            1,
            3,
        );
        assert!(
            (card.dimensions.governance - 0.9).abs() < 1e-6,
            "governance"
        );
        assert!(
            (card.dimensions.verification - 0.75).abs() < 1e-6,
            "verification"
        );
        assert!(
            (card.dimensions.reflection - 2.0 / 3.0).abs() < 1e-6,
            "reflection"
        );
        assert!((card.dimensions.review - 0.75).abs() < 1e-6, "review");
        let expected = (0.9 + 0.75 + 2.0 / 3.0 + 0.75) / 4.0;
        assert!((card.overall() - expected).abs() < 1e-6, "overall");
    }

    #[test]
    fn scorecard_reflection_clamps_and_passed_findings_ignored() {
        // reflection 计数超过失败路径数 → clamp 1.0。
        let stats = SessionStats {
            tool_failures: 1,
            verifications: 1,
            verifications_passed: 0,
            ..Default::default()
        };
        let card = Scorecard::compute("s4", &stats, &[], 5, 0, 0);
        assert_eq!(card.dimensions.reflection, 1.0);
        // passed=true 的 Blocking finding 属审计通过记录，不计入违规。
        let stats2 = SessionStats {
            tool_calls: 1,
            ..Default::default()
        };
        let card2 = Scorecard::compute("s5", &stats2, &[blocking("audit", true)], 0, 0, 0);
        assert_eq!(card2.dimensions.governance, 1.0);
    }

    #[test]
    fn scorecard_serde_roundtrip() {
        let stats = SessionStats {
            tool_calls: 2,
            ..Default::default()
        };
        let card = Scorecard::compute("s6", &stats, &[], 0, 0, 0);
        let json = serde_json::to_string(&card).unwrap();
        let back: Scorecard = serde_json::from_str(&json).unwrap();
        assert_eq!(back, card);
        assert!(json.contains("\"governance\""));
    }

    #[test]
    fn write_and_list_scorecards_skips_unrelated_and_broken_files() {
        let dir = tempfile::tempdir().unwrap();
        let mk = |id: &str, g: f32| Scorecard {
            session_id: id.to_string(),
            started_at_ms: 1,
            dimensions: ScoreDimensions {
                governance: g,
                verification: 1.0,
                reflection: 0.0,
                review: 1.0,
                protocol: 1.0,
                composite: 1.0,
            },
        };
        write_scorecard(&mk("a", 0.5), dir.path()).unwrap();
        write_scorecard(&mk("b", 1.0), dir.path()).unwrap();
        // 非评分卡 json（如 SessionReport）与解析失败的文件都应被跳过。
        std::fs::write(dir.path().join("other.json"), "{}").unwrap();
        std::fs::write(dir.path().join("bad.scorecard.json"), "{not json").unwrap();
        let cards = list_scorecards(dir.path());
        assert_eq!(cards.len(), 2);
        let ids: Vec<&str> = cards.iter().map(|c| c.session_id.as_str()).collect();
        assert!(ids.contains(&"a") && ids.contains(&"b"));
        let a = cards.iter().find(|c| c.session_id == "a").unwrap();
        assert_eq!(a.dimensions.governance, 0.5);
    }

    #[test]
    fn aggregate_scorecards_averages_and_worst_dimension() {
        let mk = |id: &str, dims: ScoreDimensions| Scorecard {
            session_id: id.to_string(),
            started_at_ms: 1,
            dimensions: dims,
        };
        let cards = vec![
            mk(
                "a",
                ScoreDimensions {
                    governance: 1.0,
                    verification: 0.5,
                    reflection: 0.0,
                    review: 0.0,
                    protocol: 0.0,
                    composite: 0.0,
                },
            ),
            mk(
                "b",
                ScoreDimensions {
                    governance: 0.0,
                    verification: 0.5,
                    reflection: 0.0,
                    review: 1.0,
                    protocol: 0.0,
                    composite: 0.0,
                },
            ),
        ];
        let agg = aggregate_scorecards(&cards);
        assert_eq!(agg.count, 2);
        assert!((agg.avg.governance - 0.5).abs() < 1e-6);
        assert!((agg.avg.verification - 0.5).abs() < 1e-6);
        assert_eq!(agg.avg.reflection, 0.0);
        assert!((agg.avg.review - 0.5).abs() < 1e-6);
        assert!((agg.avg_overall - 0.375).abs() < 1e-6);
        assert_eq!(agg.worst_dimension, "reflection");
        // 空输入：全零 + 空 worst_dimension。
        let empty = aggregate_scorecards(&[]);
        assert_eq!(empty.count, 0);
        assert_eq!(empty.worst_dimension, "");
        assert_eq!(empty.avg_overall, 0.0);
    }

    #[test]
    fn protocol_dim_boundaries() {
        // 0/0（无迁移无违规）→ 1.0
        assert_eq!(protocol_dim(0, 0), 1.0);
        // 0 违规、任意迁移数 → 1.0
        assert_eq!(protocol_dim(0, 7), 1.0);
        assert_eq!(protocol_dim(0, 1), 1.0);
        // 全违规 → 0.0
        assert_eq!(protocol_dim(3, 3), 0.0);
        // 部分违规数值正确：2/10 → 0.8
        assert!((protocol_dim(2, 10) - 0.8).abs() < 1e-6);
        // clamp 下限：违规数超过迁移数 → 0.0（而非负值）
        assert_eq!(protocol_dim(9, 3), 0.0);
        assert_eq!(protocol_dim(10, 0), 1.0); // 无迁移仍按 1.0，违规数不参与
    }

    #[test]
    fn composite_index_all_or_nothing() {
        let all_good = ScoreDimensions {
            governance: 1.0,
            verification: 1.0,
            reflection: 1.0,
            review: 1.0,
            protocol: 1.0,
            composite: 0.0,
        };
        assert!((composite_index(&all_good) - 1.0).abs() < 1e-6);
        let all_bad = ScoreDimensions {
            governance: 0.0,
            verification: 0.0,
            reflection: 0.0,
            review: 0.0,
            protocol: 0.0,
            composite: 0.0,
        };
        assert_eq!(composite_index(&all_bad), 0.0);
    }

    #[test]
    fn composite_index_weighted_correctness() {
        // 手算：0.30*1.0 + 0.25*0.5 + 0.20*0.25 + 0.15*0.0 + 0.10*1.0 = 0.625
        let dims = ScoreDimensions {
            governance: 1.0,
            verification: 0.5,
            reflection: 0.0,
            review: 1.0,
            protocol: 0.25,
            composite: 0.0,
        };
        let expected = 0.30 * 1.0 + 0.25 * 0.5 + 0.20 * 0.25 + 0.15 * 0.0 + 0.10 * 1.0;
        assert!((composite_index(&dims) - expected).abs() < 1e-6);
        // 权重常量与公式口径一致：权重合计 1.0，且按 COMPOSITE_WEIGHTS 展开结果相同。
        let via_weights: f32 = COMPOSITE_WEIGHTS
            .iter()
            .map(|(dim, w)| dim(&dims) * w)
            .sum();
        assert!((via_weights - expected).abs() < 1e-6);
        let weight_sum: f32 = COMPOSITE_WEIGHTS.iter().map(|(_, w)| w).sum();
        assert!((weight_sum - 1.0).abs() < 1e-6);
    }

    #[test]
    fn compute_fills_protocol_default_and_composite() {
        // compute 无协议统计输入：protocol 按 1.0（0/0 口径），composite 由此计算。
        let stats = SessionStats {
            tool_calls: 5,
            verifications: 2,
            verifications_passed: 2,
            ..Default::default()
        };
        let card = Scorecard::compute("s7", &stats, &[], 0, 0, 0);
        assert_eq!(card.dimensions.protocol, 1.0);
        // 0.30*1 + 0.25*1 + 0.20*1 + 0.15*0 + 0.10*1 = 0.85（reflection 无失败路径为 0）
        assert!((card.dimensions.composite - 0.85).abs() < 1e-6);
        // fill_protocol：调用方传入协议统计后 protocol/composite 更新。
        let mut card = card;
        card.fill_protocol(2, 10);
        assert!((card.dimensions.protocol - 0.8).abs() < 1e-6);
        let expected = 0.30 * 1.0 + 0.25 * 1.0 + 0.20 * 0.8 + 0.15 * 0.0 + 0.10 * 1.0;
        assert!((card.dimensions.composite - expected).abs() < 1e-6);
    }

    #[test]
    fn old_scorecard_json_deserializes_with_defaults() {
        // 旧格式 scorecard JSON（无 protocol/composite 字段）→ 新结构默认 1.0，
        // 与 compute() 无协议输入口径一致（不报错、聚合不低估）。
        let old = r#"{
            "session_id": "legacy",
            "started_at_ms": 1,
            "dimensions": {
                "governance": 0.9,
                "verification": 0.8,
                "reflection": 0.5,
                "review": 1.0
            }
        }"#;
        let card: Scorecard = serde_json::from_str(old).unwrap();
        assert_eq!(card.dimensions.protocol, 1.0);
        assert_eq!(card.dimensions.composite, 1.0);
        assert_eq!(card.dimensions.governance, 0.9);
        assert_eq!(card.dimensions.verification, 0.8);
    }

    #[test]
    fn deserialization_default_matches_compute_semantics() {
        // 口径一致性（Finding #6）：compute() 无协议输入时 protocol=1.0 且
        // composite=加权均值；旧卡反序列化缺省 protocol/composite=1.0 与之对齐，
        // 混合新旧卡聚合时旧会话不再恒 0.0。
        let stats = SessionStats {
            tool_calls: 5,
            verifications: 2,
            verifications_passed: 2,
            ..Default::default()
        };
        let computed = Scorecard::compute("s8", &stats, &[], 0, 0, 0);
        let old = r#"{
            "session_id": "legacy2",
            "started_at_ms": 1,
            "dimensions": {
                "governance": 1.0,
                "verification": 1.0,
                "reflection": 0.0,
                "review": 1.0
            }
        }"#;
        let legacy: Scorecard = serde_json::from_str(old).unwrap();
        // protocol 维口径一致：compute 无协议输入 = 1.0，旧卡缺省 = 1.0。
        assert_eq!(legacy.dimensions.protocol, computed.dimensions.protocol);
        assert_eq!(legacy.dimensions.protocol, 1.0);
        // composite 缺省按 1.0（serde 不重算；保守满分口径，聚合不低估）。
        assert_eq!(legacy.dimensions.composite, 1.0);
        // compute 侧 composite 为加权均值：0.30*1 + 0.25*1 + 0.20*1 + 0.15*0 + 0.10*1 = 0.85。
        assert!((computed.dimensions.composite - 0.85).abs() < 1e-6);
        // 混合聚合：旧卡 protocol/composite 不再以 0.0 拖累均值。
        let agg = aggregate_scorecards(&[computed.clone(), legacy.clone()]);
        assert!((agg.avg.protocol - 1.0).abs() < 1e-6);
        assert!((agg.avg.composite - 0.925).abs() < 1e-6);
    }
}
