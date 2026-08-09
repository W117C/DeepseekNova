//! Scan report: severity grouping + markdown / JSON rendering.

use crate::finding::Finding;
use crate::rule::Severity;
use deepseeknova_core::DeepseeknovaError;

/// Aggregated scan output.
pub struct ScanReport {
    findings: Vec<Finding>,
}

impl ScanReport {
    /// Build a report, sorting findings by severity (High→Low) then path.
    pub fn new(mut findings: Vec<Finding>) -> Self {
        findings.sort_by(|a, b| {
            a.severity
                .cmp(&b.severity)
                .then_with(|| a.path.cmp(&b.path))
                .then_with(|| a.line.cmp(&b.line))
        });
        Self { findings }
    }

    /// Findings without an AI verdict (skipped or --no-ai).
    pub fn uninvestigated(&self) -> usize {
        self.findings.iter().filter(|f| f.verdict.is_none()).count()
    }

    /// Render as a grouped markdown report.
    pub fn to_markdown(&self) -> String {
        let mut s = String::from("# Scan Report\n\n");
        s.push_str(&format!(
            "{} finding(s), {} uninvestigated\n\n",
            self.findings.len(),
            self.uninvestigated()
        ));
        for sev in [Severity::High, Severity::Medium, Severity::Low] {
            let group: Vec<&Finding> = self.findings.iter().filter(|f| f.severity == sev).collect();
            if group.is_empty() {
                continue;
            }
            s.push_str(&format!("## {}\n\n", sev.label()));
            for f in group {
                let verdict = match &f.verdict {
                    Some(v) if v.true_positive => format!(" ✅ TP: {}", v.note),
                    Some(v) => format!(" ⚪ FP: {}", v.note),
                    None => String::new(),
                };
                s.push_str(&format!(
                    "- `{}` {}:{} [{}]{}\n",
                    f.rule_id, f.path, f.line, f.excerpt, verdict
                ));
            }
            s.push('\n');
        }
        s
    }

    /// Render as JSON (array of findings).
    pub fn to_json(&self) -> Result<String, DeepseeknovaError> {
        Ok(serde_json::to_string_pretty(&self.findings)?)
    }

    /// Borrow findings (for CLI iteration in the process stage).
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finding::{Finding, Verdict};
    use crate::rule::Severity;

    fn f(rule: &str, sev: Severity, verdict: Option<bool>) -> Finding {
        Finding {
            rule_id: rule.into(),
            severity: sev,
            path: "a.rs".into(),
            line: 1,
            excerpt: "x".into(),
            verdict: verdict.map(|tp| Verdict {
                true_positive: tp,
                note: "n".into(),
            }),
        }
    }

    #[test]
    fn report_groups_by_severity_high_first() {
        let findings = vec![
            f("low1", Severity::Low, None),
            f("high1", Severity::High, Some(true)),
        ];
        let report = ScanReport::new(findings);
        let md = report.to_markdown();
        let hi = md.find("high1").unwrap();
        let lo = md.find("low1").unwrap();
        assert!(hi < lo, "high severity rendered before low");
    }

    #[test]
    fn report_json_roundtrips() {
        let report = ScanReport::new(vec![f("r", Severity::Medium, Some(false))]);
        let json = report.to_json().unwrap();
        assert!(json.contains("\"rule_id\""));
        assert!(json.contains("\"true_positive\""));
    }

    #[test]
    fn report_counts_unmetered_verdicts() {
        let report = ScanReport::new(vec![
            f("a", Severity::Low, None),
            f("b", Severity::Low, Some(true)),
        ]);
        assert_eq!(report.uninvestigated(), 1);
    }
}
