//! 失败回炉前的显式 LLM 反思：分析根因与修复计划，教训沉淀进记忆。
//!
//! 与 review / 蒸馏同哲学：调用失败或响应不可解析一律返回 `None`，回炉用
//! 原文案，绝不阻断循环。JSON 契约：
//! `{"root_cause":"...","fix_plan":"...","lesson":"..."}`。

use deepseeknova_core::{Message, Role};
use deepseeknova_provider::{Provider, ValidatedRequest};
use std::sync::Arc;
use tracing::warn;

/// 一次反思的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reflection {
    pub root_cause: String,
    pub fix_plan: String,
    pub lesson: String,
}

/// 反思设置（runtime 装配：provider 回落 main，max_chars 截断完成文本）。
#[derive(Clone)]
pub(crate) struct ReflectSettings {
    pub provider: Arc<dyn Provider>,
    pub max_chars: usize,
}

/// 教训沉淀钩子（runtime 注入：落 core 记忆库；None = 仅对话内）。
pub type LessonHook = Arc<dyn Fn(String) + Send + Sync>;

/// 提取首个平衡的 JSON 对象（与 review.rs 同款宽松解析；main 上该函数私有，
/// 改可见性不在任务书白名单，故本模块自带等价实现）。
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

/// 渲染反思 prompt：任务 + 失败摘要 + 最后完成文本 → 严格要求 JSON 判定。
pub fn render_reflection_prompt(task: &str, failure: &str, completion: &str) -> String {
    format!(
        "You are reflecting in the Reflect phase of the Observe → Plan → Tool → \
         Verify → Reflect → Next Action loop. The agent's last completion failed \
         verification/review. Analyze why it failed and what to change. Respond \
         with ONLY a JSON object: {{\"root_cause\": \"...\", \"fix_plan\": \
         \"...\", \"lesson\": \"one reusable lesson\"}}.\n\n\
         # Task\n{task}\n\n# Failure\n{failure}\n\n# Last completion\n{completion}"
    )
}

/// 宽松解析反思响应；缺任一字段或非法 JSON → None。
pub fn parse_reflection(raw: &str) -> Option<Reflection> {
    let json_str = extract_json(raw)?;
    let v: serde_json::Value = serde_json::from_str(&json_str).ok()?;
    let get = |key: &str| -> Option<String> {
        let s = v.get(key)?.as_str()?.trim();
        if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        }
    };
    let root_cause = get("root_cause")?;
    let fix_plan = get("fix_plan")?;
    let lesson = get("lesson")?;
    Some(Reflection {
        root_cause,
        fix_plan,
        lesson,
    })
}

/// 单次反思调用：completion 按 `max_chars` 截断；失败/不可解析返回 None。
pub async fn run_reflection(
    provider: &dyn Provider,
    task: &str,
    failure: &str,
    completion: &str,
    max_chars: usize,
) -> Option<Reflection> {
    let completion_capped: String = completion.chars().take(max_chars).collect();
    let prompt = render_reflection_prompt(task, failure, &completion_capped);
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
                "invalid reflection request ({}); skipping",
                violations.join("; ")
            );
            return None;
        }
    };
    match provider.generate(validated).await {
        Ok(out) => match parse_reflection(&out.content) {
            Some(r) => Some(r),
            None => {
                warn!("reflection response unparseable; skipping");
                None
            }
        },
        Err(e) => {
            warn!("reflection call failed ({e}); skipping");
            None
        }
    }
}

/// 回炉消息：原文案前置反思（根因 + 修复计划）。
pub fn compose_retry_message(original: &str, r: &Reflection) -> String {
    format!(
        "[Reflection] root cause: {}\nfix plan: {}\n\n{original}",
        r.root_cause, r.fix_plan
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflection_prompt_contains_contract_and_sections() {
        let p = render_reflection_prompt("fix auth", "verify failed", "done");
        for s in [
            "Reflect phase",
            "# Task",
            "# Failure",
            "# Last completion",
            "{\"root_cause\"",
        ] {
            assert!(p.contains(s), "prompt 缺少 {s}");
        }
    }

    #[test]
    fn parses_reflection_json() {
        let raw = r#"{"root_cause":"forgot to escape input","fix_plan":"escape before insert","lesson":"always escape user input"}"#;
        let r = parse_reflection(raw).unwrap();
        assert_eq!(r.root_cause, "forgot to escape input");
        assert_eq!(r.fix_plan, "escape before insert");
        assert_eq!(r.lesson, "always escape user input");

        let fenced =
            "Here:\n```json\n{\"root_cause\":\"a\",\"fix_plan\":\"b\",\"lesson\":\"c\"}\n```";
        assert_eq!(
            parse_reflection(fenced),
            Some(Reflection {
                root_cause: "a".into(),
                fix_plan: "b".into(),
                lesson: "c".into(),
            })
        );
    }

    #[test]
    fn garbage_and_missing_fields_yield_none() {
        assert_eq!(parse_reflection("not json"), None);
        assert_eq!(parse_reflection(r#"{"root_cause":"a"}"#), None);
        assert_eq!(
            parse_reflection(r#"{"root_cause":"","fix_plan":"b","lesson":"c"}"#),
            None
        );
    }

    #[test]
    fn compose_retry_message_prepends_reflection() {
        let original = "verify failed: exit 1";
        let r = Reflection {
            root_cause: "bad import".into(),
            fix_plan: "fix the import".into(),
            lesson: "check imports".into(),
        };
        let msg = compose_retry_message(original, &r);
        assert!(msg.contains("[Reflection] root cause: bad import"));
        assert!(msg.contains("fix plan: fix the import"));
        assert!(msg.ends_with(original), "原文案必须保留在尾部");
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
    async fn run_reflection_routes_success_failure_and_truncation() {
        let ok = FixedProvider {
            content: r#"{"root_cause":"a","fix_plan":"b","lesson":"c"}"#.into(),
            fail: false,
            captured: std::sync::Mutex::new(None),
        };
        let r = run_reflection(&ok, "task", "failed", "completion", 4000)
            .await
            .unwrap();
        assert_eq!(r.root_cause, "a");

        let down = FixedProvider {
            content: String::new(),
            fail: true,
            captured: std::sync::Mutex::new(None),
        };
        assert_eq!(
            run_reflection(&down, "task", "failed", "completion", 4000).await,
            None
        );

        let garbage = FixedProvider {
            content: "I'll fix it".into(),
            fail: false,
            captured: std::sync::Mutex::new(None),
        };
        assert_eq!(
            run_reflection(&garbage, "task", "failed", "completion", 4000).await,
            None
        );

        // 长完成文本按 max_chars 截断
        let cap = FixedProvider {
            content: r#"{"root_cause":"a","fix_plan":"b","lesson":"c"}"#.into(),
            fail: false,
            captured: std::sync::Mutex::new(None),
        };
        let huge = "x".repeat(10_000);
        assert_eq!(
            run_reflection(&cap, "task", "failed", &huge, 64).await,
            Some(Reflection {
                root_cause: "a".into(),
                fix_plan: "b".into(),
                lesson: "c".into(),
            })
        );
        let captured = cap.captured.lock().unwrap().clone().unwrap();
        assert!(captured.contains(&"x".repeat(64)));
        assert!(!captured.contains(&"x".repeat(65)));
    }
}
