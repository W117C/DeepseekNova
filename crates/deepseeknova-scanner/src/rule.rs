//! Scan rules: regex matchers with severity, optional language scope.

use deepseeknova_graph::parser::Lang;
use regex::Regex;
use serde::{Deserialize, Serialize};

/// Finding severity. Ordered high→low for report grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Severity {
    /// High-severity issues (e.g. hardcoded secrets, command injection).
    High,
    /// Medium-severity issues (e.g. SQL string interpolation).
    Medium,
    /// Low-severity issues (e.g. panic surfaces in non-test code).
    Low,
}

impl Severity {
    /// Stable lowercase label for CLI args / report.
    pub fn label(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }

    /// Parse a CLI severity-min argument (unknown → None).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "high" => Some(Self::High),
            "medium" => Some(Self::Medium),
            "low" => Some(Self::Low),
            _ => None,
        }
    }
}

/// A regex matcher rule. `lang = None` applies to all supported languages.
pub struct Rule {
    /// Unique rule identifier (e.g. `hardcoded-secret`).
    pub id: String,
    /// Severity assigned to findings produced by this rule.
    pub severity: Severity,
    /// Optional language scope; `None` applies to all supported languages.
    pub lang: Option<Lang>,
    /// Compiled regex matched against each source line.
    pub pattern: Regex,
    /// Human-readable description of the issue, used in scan reports.
    pub message: String,
}

#[allow(clippy::expect_used)] // builtin 常量正则；编译期 bug 由单测立即暴露
fn rule(id: &str, sev: Severity, lang: Option<Lang>, pat: &str, msg: &str) -> Rule {
    Rule {
        id: id.to_string(),
        severity: sev,
        lang,
        pattern: Regex::new(pat).expect("builtin rule regex must compile"),
        message: msg.to_string(),
    }
}

/// The P1 built-in high-signal rule set. Small and precise — the AI
/// investigation stage (process) adjudicates true/false positives.
pub fn builtin_rules() -> Vec<Rule> {
    vec![
        rule(
            "hardcoded-secret",
            Severity::High,
            None,
            r#"(?i)(api[_-]?key|secret|token|password)\s*[:=]\s*["'][^"']{8,}["']"#,
            "疑似硬编码密钥/凭据",
        ),
        rule(
            "sql-string-interpolation",
            Severity::Medium,
            None,
            r#"(?i)(SELECT|INSERT|UPDATE|DELETE)\s+.*(\{|\+|%s|\$\{)"#,
            "疑似 SQL 字符串拼接（注入面）",
        ),
        rule(
            "command-injection",
            Severity::High,
            None,
            r#"(sh\s+-c|Command::new\([^)]*\)\s*\.arg\([^)]*(\+|format!|\{))"#,
            "疑似命令注入面",
        ),
        rule(
            "rust-unwrap",
            Severity::Low,
            Some(Lang::Rust),
            r#"\.unwrap\(\)|\.expect\(|panic!\("#,
            "非测试路径的 panic 面（unwrap/expect/panic!）",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_rules_all_compile_and_nonempty() {
        let rules = builtin_rules();
        assert!(!rules.is_empty(), "must ship some builtin rules");
        for r in &rules {
            assert!(!r.id.is_empty());
            assert!(!r.message.is_empty());
        }
    }

    #[test]
    fn hardcoded_secret_rule_matches_and_rejects() {
        let rules = builtin_rules();
        let secret = rules.iter().find(|r| r.id == "hardcoded-secret").unwrap();
        assert!(secret.pattern.is_match(r#"api_key = "sk-abc123""#));
        assert!(!secret.pattern.is_match("let count = 3;"));
    }

    #[test]
    fn rust_unwrap_rule_is_rust_scoped() {
        let rules = builtin_rules();
        let unwrap = rules.iter().find(|r| r.id == "rust-unwrap").unwrap();
        assert_eq!(unwrap.lang, Some(deepseeknova_graph::parser::Lang::Rust));
        assert!(unwrap.pattern.is_match("let x = foo().unwrap();"));
    }
}
