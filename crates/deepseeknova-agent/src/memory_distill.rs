//! LLM 知识蒸馏：回合结束把 TaskObservation 提炼成可复用 skill/教训。
//!
//! 与 B3 review 同哲学：调用失败 / 响应不可解析一律返回 `None`（上层已有
//! 启发式 record_task 兜底），绝不阻断 run。JSON 契约：
//! `{"kind":"skill"|"lesson","title":"...","body":"...","tags":[...]}`。

use deepseeknova_core::memory::skill::TaskObservation;
use deepseeknova_core::{Message, Role};
use deepseeknova_provider::{Provider, ValidatedRequest};
use tracing::warn;

/// 蒸馏出的可复用知识。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistilledKnowledge {
    /// "skill"（怎么做得好）或 "lesson"（该避免什么）。
    pub kind: String,
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
}

/// 提取首个平衡的 JSON 对象（与 review.rs 同款宽松解析；review.rs 的该函数
/// 在 main 上是私有的，改可见性不在任务书白名单，故在本模块自带一份等价实现）。
fn extract_json(raw: &str) -> Option<String> {
    if let Some(start) = raw.find("```json") {
        let rest = &raw[start + 7..];
        if let Some(end) = rest.find("```") {
            return Some(rest[..end].trim().to_string());
        }
    }
    let bytes = raw.as_bytes();
    let start = raw.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if escape {
            escape = false;
            continue;
        }
        match b {
            b'\\' if in_string => escape = true,
            b'"' => in_string = !in_string,
            b'{' if !in_string => depth += 1,
            b'}' if !in_string => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(raw[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// 渲染蒸馏 prompt：任务观察 → 严格要求 JSON 判定。
pub fn render_distill_prompt(obs: &TaskObservation) -> String {
    format!(
        "You are distilling reusable knowledge in the Reflect phase of the \
         Observe → Plan → Tool → Verify → Reflect → Next Action loop. From the \
         task observation below, extract ONE reusable piece of knowledge: a \
         skill (how to do something well) or a lesson (what to avoid). Respond \
         with ONLY a JSON object: {{\"kind\": \"skill\" | \"lesson\", \
         \"title\": \"short title\", \"body\": \"concise actionable \
         knowledge\", \"tags\": [\"...\"]}}.\n\n\
         # Task\n{}\n# Steps taken\n{}\n# Tools used\n{}\n# Files touched\n{}\n\
         # Outcome\n{:?}\n# User feedback\n{}",
        obs.task_description,
        obs.steps_taken.join("\n"),
        obs.tool_calls.join(", "),
        obs.files.join(", "),
        obs.outcome,
        obs.user_feedback.clone().unwrap_or_default(),
    )
}

/// 宽松解析蒸馏响应；kind 非 skill/lesson、缺 title/body 或非法 JSON → None。
pub fn parse_distilled(raw: &str) -> Option<DistilledKnowledge> {
    let json_str = extract_json(raw)?;
    let v: serde_json::Value = serde_json::from_str(&json_str).ok()?;
    let kind = v.get("kind")?.as_str()?;
    if kind != "skill" && kind != "lesson" {
        return None;
    }
    let title = v.get("title")?.as_str()?.trim().to_string();
    let body = v.get("body")?.as_str()?.trim().to_string();
    if title.is_empty() || body.is_empty() {
        return None;
    }
    let tags = v
        .get("tags")
        .and_then(|t| t.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Some(DistilledKnowledge {
        kind: kind.to_string(),
        title,
        body,
        tags,
    })
}

/// 单次 LLM 蒸馏调用（复用 review 同款 ValidatedRequest 通路）。
/// task_description 按 `max_chars` 截断；失败/不可解析返回 None。
pub async fn run_llm_distill(
    provider: &dyn Provider,
    obs: &TaskObservation,
    max_chars: usize,
) -> Option<DistilledKnowledge> {
    let mut capped = obs.clone();
    capped.task_description = obs.task_description.chars().take(max_chars).collect();
    let prompt = render_distill_prompt(&capped);
    let msgs = vec![Message {
        role: Role::User,
        content: prompt,
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    }];
    let validated = match ValidatedRequest::new(&msgs, &[]) {
        Ok(v) => v,
        Err(violations) => {
            warn!(
                "invalid llm distill request ({}); skipping",
                violations.join("; ")
            );
            return None;
        }
    };
    match provider.generate(validated).await {
        Ok(out) => match parse_distilled(&out.content) {
            Some(k) => Some(k),
            None => {
                warn!("llm distill response unparseable; skipping");
                None
            }
        },
        Err(e) => {
            warn!("llm distill call failed ({e}); skipping");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_core::memory::skill::TaskOutcome;

    fn obs(task: &str) -> TaskObservation {
        TaskObservation {
            task_description: task.to_string(),
            tool_calls: vec!["write_file".into()],
            steps_taken: vec!["wrote file".into()],
            outcome: TaskOutcome::Success,
            user_feedback: None,
            session_id: "sess".into(),
            files: vec!["src/lib.rs".into()],
        }
    }

    #[test]
    fn distill_prompt_contains_contract_and_task() {
        let p = render_distill_prompt(&obs("fix auth"));
        for s in [
            "Reflect phase",
            "# Task",
            "# Outcome",
            "{\"kind\": \"skill\" | \"lesson\"",
        ] {
            assert!(p.contains(s), "prompt 缺少 {s}");
        }
        assert!(p.contains("fix auth"));
    }

    #[test]
    fn parses_skill_json() {
        let raw = r#"{"kind":"skill","title":"Use serde derive","body":"Prefer derive over manual impls","tags":["serde","rust"]}"#;
        let k = parse_distilled(raw).unwrap();
        assert_eq!(k.kind, "skill");
        assert_eq!(k.title, "Use serde derive");
        assert_eq!(k.body, "Prefer derive over manual impls");
        assert_eq!(k.tags, vec!["serde", "rust"]);
    }

    #[test]
    fn parses_fenced_lesson_json() {
        let raw = "Here:\n```json\n{\"kind\":\"lesson\",\"title\":\"Don't edit generated files\",\"body\":\"Regenerate instead\"}\n```";
        let k = parse_distilled(raw).unwrap();
        assert_eq!(k.kind, "lesson");
        assert_eq!(k.title, "Don't edit generated files");
    }

    #[test]
    fn garbage_and_unknown_kind_yield_none() {
        assert_eq!(parse_distilled("not json"), None);
        assert_eq!(
            parse_distilled(r#"{"kind":"tip","title":"x","body":"y"}"#),
            None
        );
        assert_eq!(
            parse_distilled(r#"{"kind":"skill","title":"","body":"y"}"#),
            None
        );
        assert_eq!(parse_distilled(r#"{"kind":"skill","title":"x"}"#), None);
    }

    struct FixedProvider {
        content: String,
        fail: bool,
        captured: std::sync::Mutex<Option<String>>,
    }

    #[async_trait::async_trait]
    impl Provider for FixedProvider {
        async fn generate(&self, validated: ValidatedRequest<'_>) -> anyhow::Result<Message> {
            *self.captured.lock().unwrap() = Some(validated.messages[0].content.clone());
            if self.fail {
                anyhow::bail!("provider down");
            }
            Ok(Message {
                role: Role::Assistant,
                content: self.content.clone(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            })
        }
    }

    #[tokio::test]
    async fn llm_distill_routes_success_failure_and_truncation() {
        let ok = FixedProvider {
            content: r#"{"kind":"skill","title":"t","body":"b"}"#.into(),
            fail: false,
            captured: std::sync::Mutex::new(None),
        };
        let k = run_llm_distill(&ok, &obs("task"), 3000).await.unwrap();
        assert_eq!(k.kind, "skill");

        let down = FixedProvider {
            content: String::new(),
            fail: true,
            captured: std::sync::Mutex::new(None),
        };
        assert_eq!(run_llm_distill(&down, &obs("task"), 3000).await, None);

        let garbage = FixedProvider {
            content: "I think so".into(),
            fail: false,
            captured: std::sync::Mutex::new(None),
        };
        assert_eq!(run_llm_distill(&garbage, &obs("task"), 3000).await, None);

        // 长任务描述按 max_chars 截断后再进 prompt
        let cap = FixedProvider {
            content: r#"{"kind":"lesson","title":"t","body":"b"}"#.into(),
            fail: false,
            captured: std::sync::Mutex::new(None),
        };
        let huge = "x".repeat(10_000);
        assert_eq!(
            run_llm_distill(&cap, &obs(&huge), 64).await,
            Some(DistilledKnowledge {
                kind: "lesson".into(),
                title: "t".into(),
                body: "b".into(),
                tags: vec![],
            })
        );
        let captured = cap.captured.lock().unwrap().clone().unwrap();
        assert!(captured.contains(&"x".repeat(64)), "任务描述应截到 64 字符");
        assert!(
            !captured.contains(&"x".repeat(65)),
            "超长部分不得进入 prompt"
        );
    }
}
