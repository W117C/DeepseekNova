//! AI investigation: adjudicate a finding true/false positive via a one-shot
//! agent run. Lenient JSON extraction — an unparseable reply yields `None`.

use crate::finding::{Finding, Verdict};
use deepseeknova_core::runner::{RunInput, Runner};

/// The investigation prompt template. The runner is expected to have file /
/// grep tools so the model can inspect surrounding code before judging.
fn build_prompt(finding: &Finding) -> String {
    format!(
        "You are a security reviewer. A regex matcher flagged a potential issue.\n\
         Rule: {rule}
File: {path}:{line}
Matched line: {excerpt}

\
         Investigate the surrounding code (read the file / grep as needed) and \
         decide whether this is a real security issue.\n\
         Reply with a single JSON object and nothing else:\n\
         \"true_positive\": <bool>, \"note\": \"<one-sentence reason>\"",
        rule = finding.rule_id,
        path = finding.path,
        line = finding.line,
        excerpt = finding.excerpt,
    )
}

/// Extract the first balanced `{...}` JSON object containing a
/// `true_positive` key and parse it. Returns `None` on any failure.
///
/// String/escape-aware scan mirrors the B3 review gate's `extract_json`
/// (crates/deepseeknova-agent/src/review.rs): braces inside string literals
/// (e.g. `"note": "a: {"`) never distort the depth, and a stray `}` is
/// clamped with `saturating_sub`. Each balanced slice is tried in turn;
/// a slice that fails to deserialize is skipped and scanning continues.
fn parse_verdict(reply: &str) -> Option<Verdict> {
    let bytes = reply.as_bytes();
    let mut start = None;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate() {
        if escape {
            escape = false;
            continue;
        }
        match b {
            b'\\' if in_string => escape = true,
            // Only track string state inside a candidate object: prefix
            // quotes (like B3's scan starting at the first `{`) are ignored.
            b'"' if start.is_some() => in_string = !in_string,
            b'{' if !in_string => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            b'}' if !in_string => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    if let Some(s) = start {
                        let slice = &reply[s..=i];
                        if let Ok(v) = serde_json::from_str::<Verdict>(slice) {
                            return Some(v);
                        }
                    }
                    start = None;
                }
            }
            _ => {}
        }
    }
    None
}

/// Investigate one finding. Returns `None` when the run errors or the reply
/// cannot be parsed into a verdict (caller records it as uninvestigated).
pub async fn investigate(finding: &Finding, runner: &dyn Runner) -> Option<Verdict> {
    let input = RunInput {
        prompt: build_prompt(finding),
        images: Vec::new(),
        model_override: None,
    };
    match runner.run(input).await {
        Ok(output) => parse_verdict(&output.text),
        Err(e) => {
            tracing::warn!(
                "investigation of {}:{} failed: {e}",
                finding.path,
                finding.line
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finding::Finding;
    use crate::rule::Severity;
    use deepseeknova_core::runner::{RunEvent, RunEventStream, RunInput, RunOutput, Runner};

    struct MockRunner {
        reply: String,
    }
    #[async_trait::async_trait]
    impl Runner for MockRunner {
        async fn run_stream(&self, _input: RunInput) -> anyhow::Result<RunEventStream> {
            let out = RunOutput {
                text: self.reply.clone(),
                tool_calls: Vec::new(),
                usage: None,
            };
            let events = vec![Ok(RunEvent::Done(out))];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    fn finding() -> Finding {
        Finding {
            rule_id: "hardcoded-secret".into(),
            severity: Severity::High,
            path: "a.rs".into(),
            line: 2,
            excerpt: "let api_key = \"sk-x\";".into(),
            verdict: None,
        }
    }

    #[tokio::test]
    async fn parses_true_positive_verdict() {
        let runner = MockRunner {
            reply: r#"Here: {"true_positive": true, "note": "real secret"}"#.into(),
        };
        let v = investigate(&finding(), &runner).await;
        assert!(v.is_some());
        let v = v.unwrap();
        assert!(v.true_positive);
        assert_eq!(v.note, "real secret");
    }

    #[tokio::test]
    async fn unparseable_reply_yields_none() {
        let runner = MockRunner {
            reply: "I could not determine anything useful.".into(),
        };
        assert!(investigate(&finding(), &runner).await.is_none());
    }

    #[tokio::test]
    async fn parses_verdict_with_unbalanced_brace_in_note() {
        // note 内含未配对花括号——字符串感知解析必须仍能取出 JSON。
        let runner = MockRunner {
            reply: r#"{"true_positive": false, "note": "template value: {"}"#.into(),
        };
        let v = investigate(&finding(), &runner).await;
        assert!(
            v.is_some(),
            "brace inside string literal must not break parsing"
        );
        assert!(!v.unwrap().true_positive);
    }
}
