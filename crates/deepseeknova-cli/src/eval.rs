//! Minimal eval harness: JSONL cases -> run output -> pass/fail report.
//!
//! Case format (one JSON object per line):
//! `{"prompt": "...", "must_contain": ["substring", ...]}`

use anyhow::Context;
use serde::Deserialize;
use std::fs;

/// A single eval case.
#[derive(Debug, Clone, Deserialize)]
pub struct EvalCase {
    pub prompt: String,
    #[serde(default)]
    pub must_contain: Vec<String>,
}

/// Load cases from a JSONL file. Blank lines and `#` comments are skipped.
pub fn load_cases(path: &str) -> anyhow::Result<Vec<EvalCase>> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read eval file {path}"))?;
    let mut cases = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let case: EvalCase = serde_json::from_str(line)
            .with_context(|| format!("eval file {path}:{} is not a valid case", idx + 1))?;
        cases.push(case);
    }
    if cases.is_empty() {
        anyhow::bail!("eval file {path} contains no cases");
    }
    Ok(cases)
}

/// A case passes when every `must_contain` substring appears in the output.
pub fn case_passes(case: &EvalCase, output: &str) -> bool {
    case.must_contain
        .iter()
        .all(|needle| output.contains(needle.as_str()))
}

/// One executed case.
pub struct EvalResult {
    pub case: EvalCase,
    pub output: String,
    pub passed: bool,
}

/// Render a markdown report (default CLI output).
pub fn render_markdown(results: &[EvalResult]) -> String {
    let total = results.len();
    let passed = results.iter().filter(|r| r.passed).count();
    let mut out = format!("# Eval report\n\n{passed}/{total} passed\n\n");
    for (i, r) in results.iter().enumerate() {
        out.push_str(&format!(
            "{}. {} — {}\n",
            i + 1,
            if r.passed { "PASS" } else { "FAIL" },
            r.case.prompt.chars().take(120).collect::<String>()
        ));
        if !r.passed {
            let preview: String = r.output.chars().take(400).collect();
            out.push_str(&format!("   output: {preview}\n"));
        }
    }
    out
}

/// Render a JSON report.
pub fn render_json(results: &[EvalResult]) -> serde_json::Value {
    let total = results.len();
    let passed = results.iter().filter(|r| r.passed).count();
    serde_json::json!({
        "total": total,
        "passed": passed,
        "failed": total - passed,
        "results": results.iter().map(|r| serde_json::json!({
            "prompt": r.case.prompt,
            "must_contain": r.case.must_contain,
            "passed": r.passed,
            "output": r.output,
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_jsonl_with_comments_and_blanks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cases.jsonl");
        std::fs::write(
            &path,
            "# comment\n{\"prompt\":\"a\",\"must_contain\":[\"x\"]}\n\n{\"prompt\":\"b\"}\n",
        )
        .unwrap();
        let cases = load_cases(path.to_str().unwrap()).unwrap();
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[1].must_contain, Vec::<String>::new());
    }

    #[test]
    fn case_passes_requires_all_substrings() {
        let case = EvalCase {
            prompt: "p".to_string(),
            must_contain: vec!["hello".to_string(), "world".to_string()],
        };
        assert!(case_passes(&case, "hello world"));
        assert!(!case_passes(&case, "hello there"));
    }

    #[test]
    fn json_report_counts_pass_fail() {
        let results = vec![
            EvalResult {
                case: EvalCase {
                    prompt: "a".to_string(),
                    must_contain: vec![],
                },
                output: "x".to_string(),
                passed: true,
            },
            EvalResult {
                case: EvalCase {
                    prompt: "b".to_string(),
                    must_contain: vec![],
                },
                output: "y".to_string(),
                passed: false,
            },
        ];
        let v = render_json(&results);
        assert_eq!(v["total"], 2);
        assert_eq!(v["passed"], 1);
        assert_eq!(v["failed"], 1);
    }
}
