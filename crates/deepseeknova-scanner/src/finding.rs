//! Scan findings and AI verdicts.

use crate::rule::Severity;
use serde::{Deserialize, Serialize};

/// AI investigation verdict for a finding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub true_positive: bool,
    pub note: String,
}

/// One matcher hit. `verdict` is filled by the process (AI) stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub rule_id: String,
    pub severity: Severity,
    pub path: String,
    pub line: usize,
    pub excerpt: String,
    pub verdict: Option<Verdict>,
}
