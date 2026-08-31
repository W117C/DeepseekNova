//! 分级评判 eval harness：JSONL 用例 → 运行 → 评分卡/成本/子串断言报告。
//!
//! 用例格式（每行一个 JSON 对象，`#` 注释与空行跳过）：
//! ```json
//! {"prompt":"...", "must_contain":["子串"], "min_score":0.8,
//!  "dimension_min":{"governance":0.9,"协议":0.8}, "cost_max":0.05, "rounds":3}
//! ```
//!
//! 断言语义（同一用例全部断言 **AND**，全过才 pass）：
//! - `must_contain`: 输出包含给定子串（保持兼容）。
//! - `min_score`: 会话评分卡综合分阈值。0..5 分制；`<= 1.0` 时按 0..1 折算
//!   （等价 `×5`），即 `min_score: 0.8` 与 `min_score: 4.0` 等价。
//! - `dimension_min.<name>`: 评分卡单维（0..1）下限；name 支持英文名
//!   (governance/verification/reflection/review/protocol/composite) 与中文别名
//!   (治理/验证/反思/审查/协议/综合)。
//! - `cost_max`: 本用例全部轮次累计 token 成本（USD）上限。
//! - `rounds`: 重试轮次上限（默认 1 = 单轮；0 视为 1）。任一轮全部断言通过即
//!   停止，记实际轮次。
//!
//! CLI 级 CI 门槛（`--require-min-score` / `--require-dimension name>=N`）见
//! [`CiThresholds`]；任一门槛未达 → [`eval_exit_code`] 非零退出。

use deepseeknova_metrics::{ScoreDimensions, Scorecard};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;

/// 单条 eval 用例。
#[derive(Debug, Clone, Deserialize)]
pub struct EvalCase {
    pub prompt: String,
    /// 用例名（报告可读性；缺省用 "case N"）。
    #[serde(default)]
    pub name: Option<String>,
    /// 子串断言：输出包含每个元素（保持）。
    #[serde(default)]
    pub must_contain: Vec<String>,
    /// 综合分阈值：评分卡 composite。0..5 分制；`<= 1.0` 时按 0..1 折算（×5）。
    #[serde(default)]
    pub min_score: Option<f32>,
    /// 单维阈值：维度名 → 0..1 下限（AND 语义）。
    #[serde(default)]
    pub dimension_min: HashMap<String, f32>,
    /// 成本上限（USD）：本用例全部轮次累计成本不得超过。
    #[serde(default)]
    pub cost_max: Option<f64>,
    /// 重试轮次上限（默认 1 = 单轮；0 视为 1）。
    #[serde(default)]
    pub rounds: u32,
}

impl EvalCase {
    /// 用例展示名：`name` 优先，否则 `case <idx+1>`。
    pub fn label(&self, idx: usize) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| format!("case {}", idx + 1))
    }

    /// 实际轮次：`rounds.max(1)`（0 = 单轮）。
    pub fn effective_rounds(&self) -> u32 {
        self.rounds.max(1)
    }
}

/// 一次 run 的实测值（断言输入；由 metrics hook 捕获的评分卡 + 成本台账 + 输出）。
#[derive(Debug, Clone, Default)]
pub struct CaseValues {
    pub output: String,
    /// 本轮会话评分卡（run 结束 metrics hook 捕获；run 失败时为 None）。
    pub card: Option<Scorecard>,
    /// 本用例已执行轮次的累计成本（USD）；无单价/未计量时为 None。
    pub cost_usd: Option<f64>,
    /// F1：本用例累计的前缀缓存命中率（0..1，跨全部行的 hit/miss 汇总）；
    /// 无缓存记账（hit+miss 均为 0）时为 None（门禁仅对缓存端点生效）。
    pub cache_hit_rate: Option<f64>,
}

impl CaseValues {
    /// 综合分（0..5 分制 = composite × 5）。
    pub fn score_0_5(&self) -> Option<f32> {
        self.card.as_ref().map(|c| c.dimensions.composite * 5.0)
    }

    /// 评分卡各维（0..1）。
    pub fn dimensions(&self) -> Option<ScoreDimensions> {
        self.card.as_ref().map(|c| c.dimensions)
    }
}

/// 单条断言结果。
#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    /// 断言类型：contains / score / dimension / cost。
    pub kind: &'static str,
    /// 人类可读断言描述（含阈值）。
    pub description: String,
    /// 实际值描述（断言无数据基础时说明原因，不伪造数值）。
    pub actual: String,
    pub passed: bool,
}

/// 评估单条用例：全部断言 AND 语义。
pub fn evaluate_case(case: &EvalCase, values: &CaseValues) -> Vec<CheckResult> {
    let mut checks = Vec::new();
    for needle in &case.must_contain {
        let found = values.output.contains(needle.as_str());
        checks.push(CheckResult {
            kind: "contains",
            description: format!("must_contain `{needle}`"),
            actual: if found {
                "found".to_string()
            } else {
                "missing".to_string()
            },
            passed: found,
        });
    }
    if let Some(threshold) = case.min_score {
        // 0..5 分制；阈值 <= 1.0 视为 0..1 折算 ×5。
        let t = if threshold > 1.0 {
            threshold
        } else {
            threshold * 5.0
        };
        let desc = format!("score >= {t:.2}/5（综合分）");
        match values.score_0_5() {
            Some(actual) => checks.push(CheckResult {
                kind: "score",
                description: desc,
                actual: format!("{actual:.2}/5"),
                passed: actual + 1e-3 >= t,
            }),
            None => checks.push(CheckResult {
                kind: "score",
                description: desc,
                actual: "评分卡不可用".to_string(),
                passed: false,
            }),
        }
    }
    for (name, threshold) in &case.dimension_min {
        let desc = format!("dimension.{name} >= {threshold:.2}");
        match values.dimensions().and_then(|d| d.get(name)) {
            Some(actual) => checks.push(CheckResult {
                kind: "dimension",
                description: desc,
                actual: format!("{actual:.2}"),
                passed: actual + 1e-3 >= *threshold,
            }),
            None => checks.push(CheckResult {
                kind: "dimension",
                description: desc,
                actual: "维度不可用".to_string(),
                passed: false,
            }),
        }
    }
    if let Some(limit) = case.cost_max {
        let desc = format!("cost <= {limit:.4} USD");
        match values.cost_usd {
            Some(actual) => checks.push(CheckResult {
                kind: "cost",
                description: desc,
                actual: format!("{actual:.4} USD"),
                passed: actual <= limit,
            }),
            None => checks.push(CheckResult {
                kind: "cost",
                description: desc,
                actual: "成本不可用".to_string(),
                passed: false,
            }),
        }
    }
    checks
}

/// 一次用例的执行结果（可能多轮）。
#[derive(Debug, Clone, Serialize)]
pub struct EvalResult {
    pub name: String,
    pub prompt: String,
    pub passed: bool,
    pub checks: Vec<CheckResult>,
    /// 最终采用的会话评分卡（多轮取命中轮，无命中轮取最后一轮）。
    pub card: Option<Scorecard>,
    /// 本用例全部执行轮次累计成本（USD）。
    pub cost_usd: Option<f64>,
    /// F1：本用例累计前缀缓存命中率（0..1）；无缓存记账为 None。
    pub cache_hit_rate: Option<f64>,
    /// 实际执行轮次（>=1）。
    pub rounds: u32,
    pub output: String,
    /// run 级错误（如 provider 调用失败）；非 None 时 passed=false。
    pub error: Option<String>,
}

/// 多轮重试选择：返回 `(采用轮索引, 实际执行轮次)`。
/// 语义：最多 `round_passed.len()` 轮，一旦某轮全部断言通过即停；
/// 取首个通过轮；无通过轮时取最后一轮。`round_passed` 为空时视为单轮。
pub fn select_round(round_passed: &[bool]) -> (usize, usize) {
    if round_passed.is_empty() {
        return (0, 1);
    }
    let idx = round_passed
        .iter()
        .position(|p| *p)
        .unwrap_or(round_passed.len() - 1);
    (idx, idx + 1)
}

/// CI 门槛（命令行级）。任一门槛未达 → 报告区分并退出非零。
#[derive(Debug, Clone, Default)]
pub struct CiThresholds {
    /// 全部用例综合分均值下限（0..5；`<= 1.0` 按 0..1 折算 ×5）。
    pub min_score: Option<f32>,
    /// 单维均值下限（维度名 → 0..1）。
    pub dimension_min: Vec<(String, f32)>,
    /// 全部用例前缀缓存命中率均值（0..1）下限。均值基于有缓存记账的
    /// 用例；**全部用例无记账时门槛跳过（n/a 通过）**——兼容无缓存端点
    /// （命中率 0 与"无缓存数据"是两回事，前者照常参与均值并判失败）。
    pub min_cache_hit_rate: Option<f32>,
}

/// 单条 CI 门槛检查结果。
#[derive(Debug, Clone, Serialize)]
pub struct CiCheck {
    /// 门槛名（score / dimension.&lt;name&gt;）。
    pub label: String,
    pub threshold: f32,
    /// 实际均值；无评分卡数据时为 None。
    pub actual: Option<f32>,
    pub passed: bool,
}

impl CiCheck {
    fn actual_display(&self) -> String {
        self.actual
            .map(|a| format!("{a:.2}"))
            .unwrap_or_else(|| "n/a".to_string())
    }
}

/// CI 门槛汇总。
#[derive(Debug, Clone, Default, Serialize)]
pub struct CiSummary {
    pub checks: Vec<CiCheck>,
    pub passed: bool,
}

/// 依据聚合均值检查 CI 门槛。
pub fn check_ci(
    thresholds: &CiThresholds,
    avg_score_0_5: Option<f32>,
    avg_dims: Option<&ScoreDimensions>,
    avg_cache_hit_rate: Option<f32>,
) -> CiSummary {
    let mut checks = Vec::new();
    if let Some(threshold) = thresholds.min_score {
        let t = if threshold > 1.0 {
            threshold
        } else {
            threshold * 5.0
        };
        let actual = avg_score_0_5;
        let passed = actual.is_some_and(|a| a + 1e-3 >= t);
        checks.push(CiCheck {
            label: "score".to_string(),
            threshold: t,
            actual,
            passed,
        });
    }
    if let Some(threshold) = thresholds.min_cache_hit_rate {
        let passed = match avg_cache_hit_rate {
            // 有缓存记账 → 均值判门槛（容差同其他门槛）。
            Some(actual) => actual + 1e-3 >= threshold,
            // 全部用例无缓存记账 → n/a 跳过（见 CiThresholds 字段说明）。
            None => true,
        };
        checks.push(CiCheck {
            label: "cache_hit_rate".to_string(),
            threshold,
            actual: avg_cache_hit_rate,
            passed,
        });
    }
    for (name, threshold) in &thresholds.dimension_min {
        let actual = avg_dims.and_then(|d| d.get(name));
        let passed = actual.is_some_and(|a| a + 1e-3 >= *threshold);
        checks.push(CiCheck {
            label: format!("dimension.{name}"),
            threshold: *threshold,
            actual,
            passed,
        });
    }
    let passed = checks.iter().all(|c| c.passed);
    CiSummary { checks, passed }
}

/// 全部用例 + CI 门槛汇总。
#[derive(Debug, Clone, Serialize)]
pub struct EvalSummary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    /// 全部用例综合分均值（0..5；基于有评分卡的用例，无卡用例不参与）。
    pub avg_score_0_5: Option<f32>,
    /// 各维均值（0..1；同上）。
    pub avg_dimensions: Option<ScoreDimensions>,
    /// 前缀缓存命中率均值（0..1；仅含有记账的用例；全无记账 → None）。
    pub avg_cache_hit_rate: Option<f32>,
    pub ci: CiSummary,
}

/// 汇总全部结果 + 检查 CI 门槛。
pub fn summarize(results: &[EvalResult], thresholds: CiThresholds) -> EvalSummary {
    let total = results.len();
    let passed = results.iter().filter(|r| r.passed).count();
    let failed = total - passed;
    let cards: Vec<&Scorecard> = results.iter().filter_map(|r| r.card.as_ref()).collect();
    let avg_dims = average_dims(&cards);
    let avg_score_0_5 = avg_dims.as_ref().map(|d| d.composite * 5.0);
    // 命中率均值仅基于有缓存记账的用例（None 不参与）；全空 → n/a。
    let avg_cache_hit_rate: Option<f32> = {
        let rates: Vec<f64> = results.iter().filter_map(|r| r.cache_hit_rate).collect();
        if rates.is_empty() {
            None
        } else {
            Some((rates.iter().sum::<f64>() / rates.len() as f64) as f32)
        }
    };
    let ci = check_ci(
        &thresholds,
        avg_score_0_5,
        avg_dims.as_ref(),
        avg_cache_hit_rate,
    );
    EvalSummary {
        total,
        passed,
        failed,
        avg_score_0_5,
        avg_dimensions: avg_dims,
        avg_cache_hit_rate,
        ci,
    }
}

/// 各维均值（空输入 → None）。
fn average_dims(cards: &[&Scorecard]) -> Option<ScoreDimensions> {
    if cards.is_empty() {
        return None;
    }
    let n = cards.len() as f32;
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
    Some(ScoreDimensions {
        governance: sum.governance / n,
        verification: sum.verification / n,
        reflection: sum.reflection / n,
        review: sum.review / n,
        protocol: sum.protocol / n,
        composite: sum.composite / n,
    })
}

/// 由汇总决定进程退出码（供 CI 门禁）：
/// - `0`：全部用例通过且 CI 门槛满足；
/// - `1`：仅条目级失败（有用例未通过，CI 门槛满足）；
/// - `2`：仅 CI 门槛失败（用例全过但均值未达门槛）；
/// - `3`：两者皆有。
pub fn eval_exit_code(summary: &EvalSummary) -> i32 {
    let case_failed = summary.failed > 0;
    let ci_failed = !summary.ci.passed;
    match (case_failed, ci_failed) {
        (false, false) => 0,
        (true, false) => 1,
        (false, true) => 2,
        (true, true) => 3,
    }
}

/// Load cases from a JSONL file. Blank lines and `#` comments are skipped.
pub fn load_cases(path: &str) -> Result<Vec<EvalCase>, deepseeknova_core::DeepseeknovaError> {
    let text = fs::read_to_string(path).map_err(|e| {
        deepseeknova_core::DeepseeknovaError::Io(std::io::Error::other(format!(
            "failed to read eval file {path}: {e}"
        )))
    })?;
    let mut cases = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let case: EvalCase = serde_json::from_str(line).map_err(|e| {
            deepseeknova_core::DeepseeknovaError::config(format!(
                "eval file {path}:{} is not a valid case: {e}",
                idx + 1
            ))
        })?;
        cases.push(case);
    }
    if cases.is_empty() {
        return Err(deepseeknova_core::DeepseeknovaError::config(format!(
            "eval file {path} contains no cases"
        )));
    }
    Ok(cases)
}

/// Render a markdown report (default CLI output).
pub fn render_markdown(results: &[EvalResult], summary: &EvalSummary) -> String {
    let mut out = format!(
        "# Eval report\n\n{}/{} passed",
        summary.passed, summary.total
    );
    if let Some(avg) = summary.avg_score_0_5 {
        out.push_str(&format!(" · 综合分均值 {avg:.2}/5"));
    }
    out.push('\n');
    for (i, r) in results.iter().enumerate() {
        out.push_str(&format!(
            "\n{}. {} — {}\n",
            i + 1,
            if r.passed { "PASS" } else { "FAIL" },
            r.name
        ));
        for c in &r.checks {
            out.push_str(&format!(
                "   - {}: {}（actual {}）{}\n",
                c.kind,
                c.description,
                c.actual,
                if c.passed { "✓" } else { "✗" }
            ));
        }
        if let Some(card) = &r.card {
            let d = &card.dimensions;
            out.push_str(&format!(
                "   score {:.2}/5 · dimensions 治理 {:.2} 验证 {:.2} 反思 {:.2} 审查 {:.2} 协议 {:.2}\n",
                d.composite * 5.0,
                d.governance,
                d.verification,
                d.reflection,
                d.review,
                d.protocol
            ));
        }
        if let Some(c) = r.cost_usd {
            out.push_str(&format!("   cost {c:.4} USD\n"));
        }
        if let Some(rate) = r.cache_hit_rate {
            out.push_str(&format!("   prefix cache hit rate: {:.1}%\n", rate * 100.0));
        }
        out.push_str(&format!("   rounds: {}\n", r.rounds));
        if let Some(err) = &r.error {
            out.push_str(&format!("   error: {err}\n"));
        }
        if !r.passed && r.error.is_none() {
            let preview: String = r.output.chars().take(400).collect();
            out.push_str(&format!("   output: {preview}\n"));
        }
    }
    out.push_str("\n## CI 门槛\n");
    if summary.ci.checks.is_empty() {
        out.push_str("- （未设置）\n");
    }
    for c in &summary.ci.checks {
        out.push_str(&format!(
            "- {} >= {:.2}（actual {}）{}\n",
            c.label,
            c.threshold,
            c.actual_display(),
            if c.passed { "PASS" } else { "FAIL" }
        ));
    }
    out
}

/// Render a JSON report.
pub fn render_json(results: &[EvalResult], summary: &EvalSummary) -> serde_json::Value {
    serde_json::json!({
        "total": summary.total,
        "passed": summary.passed,
        "failed": summary.failed,
        "avg_score_0_5": summary.avg_score_0_5,
        "avg_dimensions": summary.avg_dimensions,
        "ci": summary.ci,
        "exit_code": eval_exit_code(summary),
        "results": results.iter().map(|r| serde_json::json!({
            "name": r.name,
            "prompt": r.prompt,
            "passed": r.passed,
            "rounds": r.rounds,
            "score_0_5": r.card.as_ref().map(|c| c.dimensions.composite * 5.0),
            "dimensions": r.card.as_ref().map(|c| &c.dimensions),
            "cost_usd": r.cost_usd,
            "cache_hit_rate": r.cache_hit_rate,
            "checks": r.checks,
            "error": r.error,
            "output": r.output,
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(composite: f32, governance: f32) -> Scorecard {
        Scorecard {
            session_id: "s".into(),
            started_at_ms: 0,
            dimensions: ScoreDimensions {
                governance,
                verification: 0.8,
                reflection: 0.7,
                review: 0.6,
                protocol: 0.5,
                composite,
            },
            first_pass: false,
            retry_rounds: 0,
            cache_hit_rate: None,
        }
    }

    fn values(output: &str, card: Option<Scorecard>, cost: Option<f64>) -> CaseValues {
        CaseValues {
            output: output.into(),
            card,
            cost_usd: cost,
            cache_hit_rate: None,
        }
    }

    #[test]
    fn loads_jsonl_with_comments_and_blanks_and_graded_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cases.jsonl");
        std::fs::write(
            &path,
            "# comment\n{\"prompt\":\"a\",\"must_contain\":[\"x\"]}\n\n{\"prompt\":\"b\",\"min_score\":4.0,\"dimension_min\":{\"governance\":0.9},\"cost_max\":0.05,\"rounds\":3,\"name\":\"B\"}\n",
        )
        .unwrap();
        let cases = load_cases(path.to_str().unwrap()).unwrap();
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[1].must_contain, Vec::<String>::new());
        assert_eq!(cases[1].min_score, Some(4.0));
        assert_eq!(cases[1].dimension_min.get("governance"), Some(&0.9));
        assert_eq!(cases[1].cost_max, Some(0.05));
        assert_eq!(cases[1].rounds, 3);
        assert_eq!(cases[1].name.as_deref(), Some("B"));
        // 缺省轮次 = 0 → effective_rounds 回落单轮。
        assert_eq!(cases[0].effective_rounds(), 1);
        assert_eq!(cases[1].effective_rounds(), 3);
        assert_eq!(cases[1].label(1), "B");
        assert_eq!(cases[0].label(0), "case 1");
    }

    #[test]
    fn contains_assertion_positive_and_negative() {
        let case = EvalCase {
            prompt: "p".into(),
            name: None,
            must_contain: vec!["hello".into(), "world".into()],
            min_score: None,
            dimension_min: HashMap::new(),
            cost_max: None,
            rounds: 0,
        };
        let ok = evaluate_case(&case, &values("hello world", None, None));
        assert!(ok.iter().all(|c| c.passed));
        let bad = evaluate_case(&case, &values("hello there", None, None));
        assert_eq!(bad.len(), 2);
        assert!(bad[0].passed); // hello found
        assert!(!bad[1].passed); // world missing
    }

    #[test]
    fn score_assertion_0_to_5_and_0_to_1_scales() {
        let case_5 = EvalCase {
            prompt: "p".into(),
            name: None,
            must_contain: vec![],
            min_score: Some(4.0), // 0..5 分制
            dimension_min: HashMap::new(),
            cost_max: None,
            rounds: 0,
        };
        // composite 0.85 → 4.25/5 ≥ 4.0 ✓
        let pass = evaluate_case(&case_5, &values("", Some(card(0.85, 1.0)), None));
        assert!(pass[0].passed, "{:?}", pass[0]);
        // composite 0.75 → 3.75/5 < 4.0 ✗
        let fail = evaluate_case(&case_5, &values("", Some(card(0.75, 1.0)), None));
        assert!(!fail[0].passed);
        // 0..1 折算：min_score 0.8 ≡ 4.0/5。
        let case_1 = EvalCase {
            min_score: Some(0.8),
            ..case_5.clone()
        };
        let pass2 = evaluate_case(&case_1, &values("", Some(card(0.85, 1.0)), None));
        assert!(pass2[0].passed);
        let fail2 = evaluate_case(&case_1, &values("", Some(card(0.75, 1.0)), None));
        assert!(!fail2[0].passed);
        // 无评分卡 → 断言 fail（说明原因）。
        let noc = evaluate_case(&case_5, &values("", None, None));
        assert!(!noc[0].passed);
        assert_eq!(noc[0].actual, "评分卡不可用");
    }

    #[test]
    fn dimension_assertion_by_name_and_alias() {
        // card() 助手 protocol 固定 0.5，这里构造 protocol 可调的卡片。
        let mk_card = |composite: f32, governance: f32, protocol: f32| Scorecard {
            dimensions: ScoreDimensions {
                governance,
                protocol,
                ..card(composite, governance).dimensions
            },
            ..card(composite, governance)
        };
        let case = EvalCase {
            prompt: "p".into(),
            name: None,
            must_contain: vec![],
            min_score: None,
            dimension_min: HashMap::from([
                ("governance".to_string(), 0.9),
                ("协议".to_string(), 0.8),
            ]),
            cost_max: None,
            rounds: 0,
        };
        // 全部达标（governance 0.95 ≥ 0.9，协议 0.85 ≥ 0.8）。
        let pass = evaluate_case(&case, &values("", Some(mk_card(1.0, 0.95, 0.85)), None));
        assert_eq!(pass.len(), 2);
        assert!(pass.iter().all(|c| c.passed), "{:?}", pass);
        // 单维不足：governance 0.50 < 0.90 与 协议 0.50 < 0.80 均失败。
        let fail = evaluate_case(&case, &values("", Some(mk_card(1.0, 0.5, 0.5)), None));
        assert!(fail.iter().all(|c| !c.passed), "{:?}", fail);
        // 未知维度名 → 无法取值，断言 fail。
        let unknown = EvalCase {
            dimension_min: HashMap::from([("nope".to_string(), 0.5)]),
            ..case.clone()
        };
        let bad = evaluate_case(&unknown, &values("", Some(mk_card(1.0, 1.0, 1.0)), None));
        assert!(!bad[0].passed);
        assert_eq!(bad[0].actual, "维度不可用");
    }

    #[test]
    fn cost_assertion_limits_and_unavailable() {
        let case = EvalCase {
            prompt: "p".into(),
            name: None,
            must_contain: vec![],
            min_score: None,
            dimension_min: HashMap::new(),
            cost_max: Some(0.05),
            rounds: 0,
        };
        let pass = evaluate_case(&case, &values("", None, Some(0.032)));
        assert!(pass[0].passed);
        let fail = evaluate_case(&case, &values("", None, Some(0.12)));
        assert!(!fail[0].passed);
        // 成本不可用（未计量/无单价）→ 断言 fail 并说明原因。
        let noc = evaluate_case(&case, &values("", None, None));
        assert!(!noc[0].passed);
        assert_eq!(noc[0].actual, "成本不可用");
    }

    #[test]
    fn multiple_assertions_and_semantics() {
        let case = EvalCase {
            prompt: "p".into(),
            name: None,
            must_contain: vec!["answer".into()],
            min_score: Some(4.0),
            dimension_min: HashMap::from([("governance".to_string(), 0.9)]),
            cost_max: Some(0.05),
            rounds: 0,
        };
        let all_pass = evaluate_case(&case, &values("answer", Some(card(0.9, 0.95)), Some(0.03)));
        assert_eq!(all_pass.len(), 4);
        assert!(all_pass.iter().all(|c| c.passed));
        // 任一个断言失败 → 用例整体失败。
        let contains_fail =
            evaluate_case(&case, &values("nope", Some(card(0.9, 0.95)), Some(0.03)));
        assert!(!contains_fail.iter().all(|c| c.passed));
        let cost_fail = evaluate_case(&case, &values("answer", Some(card(0.9, 0.95)), Some(0.09)));
        assert!(!cost_fail.iter().all(|c| c.passed));
    }

    #[test]
    fn select_round_picks_first_pass_or_last() {
        assert_eq!(select_round(&[true]), (0, 1));
        assert_eq!(select_round(&[false, true, false]), (1, 2));
        assert_eq!(select_round(&[false, false, false]), (2, 3));
        assert_eq!(select_round(&[]), (0, 1));
        // 首轮即过 → 只执行一轮。
        assert_eq!(select_round(&[true, true]), (0, 1));
    }

    #[test]
    fn ci_thresholds_score_and_dimension() {
        let thresholds = CiThresholds {
            min_score: Some(3.5), // 0..5
            dimension_min: vec![("governance".to_string(), 0.85)],
            min_cache_hit_rate: None,
        };
        // 达标。
        let pass = check_ci(
            &thresholds,
            Some(4.0),
            Some(&card(0.8, 0.9).dimensions),
            None,
        );
        assert!(pass.passed, "{:?}", pass.checks);
        // 综合分不足。
        let fail_score = check_ci(
            &thresholds,
            Some(3.0),
            Some(&card(0.6, 0.9).dimensions),
            None,
        );
        assert!(!fail_score.passed);
        assert!(!fail_score.checks[0].passed);
        // 单维不足（中文别名）。
        let fail_dim = check_ci(
            &thresholds,
            Some(4.0),
            Some(&card(0.8, 0.5).dimensions),
            None,
        );
        assert!(!fail_dim.passed);
        assert!(!fail_dim.checks[1].passed);
        assert_eq!(fail_dim.checks[1].label, "dimension.governance");
        // 无评分卡数据 → 门槛 fail（n/a）。
        let no_card = check_ci(&thresholds, None, None, None);
        assert!(!no_card.passed);
        assert_eq!(no_card.checks[0].actual, None);
        // 未设置门槛 → 恒通过。
        let empty = check_ci(&CiThresholds::default(), None, None, None);
        assert!(empty.passed);
        assert!(empty.checks.is_empty());
    }

    /// cache_hit_rate 门槛：有记账判均值；全无记账 n/a 跳过；0 参与判失败。
    #[test]
    fn ci_cache_hit_rate_gate_semantics() {
        let t = CiThresholds {
            min_score: None,
            dimension_min: vec![],
            min_cache_hit_rate: Some(0.7),
        };
        let s = check_ci(&t, None, None, Some(0.85));
        assert!(s.passed);
        let s = check_ci(&t, None, None, Some(0.5));
        assert!(!s.passed);
        // 全部用例无缓存记账 → n/a 跳过（不误伤无缓存端点）。
        let s = check_ci(&t, None, None, None);
        assert!(s.passed);
        assert_eq!(s.checks[0].label, "cache_hit_rate");
    }

    #[test]
    fn ci_threshold_0_to_1_scale_normalizes_to_5() {
        // min_score 0.7（0..1 折算 3.5/5）与 3.5（0..5）等价。
        let t_01 = CiThresholds {
            min_score: Some(0.7),
            dimension_min: vec![],
            min_cache_hit_rate: None,
        };
        let pass = check_ci(&t_01, Some(3.6), Some(&card(0.72, 1.0).dimensions), None);
        assert!(pass.passed);
        assert!((pass.checks[0].threshold - 3.5).abs() < 1e-3);
        let fail = check_ci(&t_01, Some(3.4), Some(&card(0.68, 1.0).dimensions), None);
        assert!(!fail.passed);
    }

    #[test]
    fn summarize_counts_and_averages_over_cards() {
        let results = vec![
            EvalResult {
                name: "a".into(),
                prompt: "p".into(),
                passed: true,
                checks: vec![],
                card: Some(card(0.8, 0.9)),
                cost_usd: Some(0.01),
                cache_hit_rate: Some(0.95),
                rounds: 1,
                output: "x".into(),
                error: None,
            },
            EvalResult {
                name: "b".into(),
                prompt: "q".into(),
                passed: false,
                checks: vec![],
                card: Some(card(0.6, 0.5)),
                cost_usd: None,
                cache_hit_rate: None,
                rounds: 2,
                output: "y".into(),
                error: None,
            },
            // 无评分卡（run 失败）→ 不参与均值。
            EvalResult {
                name: "c".into(),
                prompt: "r".into(),
                passed: false,
                checks: vec![],
                card: None,
                cost_usd: None,
                cache_hit_rate: None,
                rounds: 1,
                output: "".into(),
                error: Some("boom".into()),
            },
        ];
        let summary = summarize(&results, CiThresholds::default());
        assert_eq!(summary.total, 3);
        assert_eq!(summary.passed, 1);
        assert_eq!(summary.failed, 2);
        // 均值基于有卡的两例：composite (0.8+0.6)/2=0.7 → 3.5/5。
        assert!((summary.avg_score_0_5.unwrap() - 3.5).abs() < 1e-3);
        let dims = summary.avg_dimensions.unwrap();
        assert!((dims.governance - 0.7).abs() < 1e-3);
        assert!(summary.ci.passed);
    }

    #[test]
    fn exit_code_matrix() {
        let base = EvalSummary {
            total: 2,
            passed: 2,
            failed: 0,
            avg_score_0_5: Some(4.0),
            avg_dimensions: None,
            avg_cache_hit_rate: None,
            ci: CiSummary {
                checks: vec![],
                passed: true,
            },
        };
        assert_eq!(eval_exit_code(&base), 0);
        // 仅条目级失败 → 1。
        let case_fail = EvalSummary {
            passed: 1,
            failed: 1,
            ..base.clone()
        };
        assert_eq!(eval_exit_code(&case_fail), 1);
        // 仅 CI 门槛失败 → 2。
        let ci_fail = EvalSummary {
            ci: CiSummary {
                checks: vec![],
                passed: false,
            },
            ..base
        };
        assert_eq!(eval_exit_code(&ci_fail), 2);
        // 两者皆有 → 3。
        let both = EvalSummary {
            passed: 1,
            failed: 1,
            ci: CiSummary {
                checks: vec![],
                passed: false,
            },
            ..base
        };
        assert_eq!(eval_exit_code(&both), 3);
    }

    #[test]
    fn json_report_contains_graded_fields_and_exit_code() {
        let results = vec![EvalResult {
            name: "a".into(),
            prompt: "p".into(),
            passed: true,
            checks: vec![CheckResult {
                kind: "score",
                description: "score >= 4.00/5".into(),
                actual: "4.25/5".into(),
                passed: true,
            }],
            card: Some(card(0.85, 1.0)),
            cost_usd: Some(0.02),
            cache_hit_rate: Some(0.9),
            rounds: 2,
            output: "x".into(),
            error: None,
        }];
        let summary = summarize(&results, CiThresholds::default());
        let v = render_json(&results, &summary);
        assert_eq!(v["total"], 1);
        assert_eq!(v["passed"], 1);
        assert_eq!(v["failed"], 0);
        assert_eq!(v["exit_code"], 0);
        assert_eq!(v["results"][0]["score_0_5"], 4.25);
        assert_eq!(v["results"][0]["rounds"], 2);
        assert_eq!(v["results"][0]["checks"][0]["passed"], true);
        assert!(v["results"][0]["dimensions"]["governance"].is_number());
        assert_eq!(v["results"][0]["cost_usd"], 0.02);
        assert_eq!(v["results"][0]["cache_hit_rate"], 0.9);
    }
}
