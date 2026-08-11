//! Scan findings and AI verdicts.

use crate::rule::Severity;
use serde::{Deserialize, Serialize};

/// AI investigation verdict for a finding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    /// Whether the finding is a real security issue (as opposed to a false positive).
    pub true_positive: bool,
    /// One-sentence human-readable explanation from the AI investigator.
    pub note: String,
}

/// One matcher hit. `verdict` is filled by the process (AI) stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    /// Identifier of the matcher rule that produced this finding.
    pub rule_id: String,
    /// Severity assigned by the rule.
    pub severity: Severity,
    /// Workspace-relative path of the scanned file.
    pub path: String,
    /// 1-based line number of the matched line.
    pub line: usize,
    /// Trimmed text of the matched line (first 200 chars).
    pub excerpt: String,
    /// AI verdict, filled by the process (investigation) stage.
    pub verdict: Option<Verdict>,
}
