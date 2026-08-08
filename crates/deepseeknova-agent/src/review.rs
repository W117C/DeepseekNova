//! B3 完成前自审：run 收尾发 Done 前，用廉价模型审查本轮 git diff 与任务
//! 文本，宽松解析 {verdict, issues}。非 git / diff 失败 / LLM 失败 / 解析
//! 失败一律优雅降级（跳过审查，warn），绝不阻断 Done。

use deepseeknova_core::{Message, Role};
use deepseeknova_provider::{Provider, ValidatedRequest};
use std::path::Path;
use tracing::warn;

/// 审查配置（runtime 从 `[review]` 配置段装配）。
pub(crate) struct ReviewSettings {
    pub diff_cap_tokens: usize,
    pub max_cycles: usize,
}

/// 审查判定结果。
#[derive(Debug, PartialEq)]
pub(crate) enum Verdict {
    Approve,
    Issues(Vec<String>),
}

/// 采集 git diff（--stat + 正文，正文按 cap 截断）。非 git 仓库或命令失败
/// 返回 None（调用方跳过审查）。
pub(crate) async fn collect_diff(workspace_root: &Path, cap_chars: usize) -> Option<String> {
    let in_repo = tokio::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(workspace_root)
        .output()
        .await
        .ok()?;
    if !in_repo.status.success() {
        return None;
    }
    // 对 HEAD 做 diff：覆盖未暂存 + 已暂存改动（agent 经 shell 执行过 git add 时裸
    // `git diff` 会漏审）。
    let stat = git_capture(workspace_root, &["diff", "--stat", "HEAD"]).await?;
    let body = git_capture(workspace_root, &["diff", "HEAD"]).await?;
    if stat.trim().is_empty() && body.trim().is_empty() {
        return None; // 无改动可审
    }
    let capped: String = body.chars().take(cap_chars).collect();
    Some(format!(
        "## diff --stat\n{stat}\n## diff (capped)\n{capped}"
    ))
}

async fn git_capture(root: &Path, args: &[&str]) -> Option<String> {
    let out = tokio::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        warn!("git {args:?} failed during review; skipping");
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 渲染审查 prompt：diff + 任务文本 + 完成声明 → 严格要求 JSON 判定。
pub(crate) fn render_review_prompt(task: &str, completion: &str, diff: &str) -> String {
    format!(
        "You are a strict but fair reviewer operating in the Reflect phase of \
         the Observe → Plan → Tool → Verify → Reflect → Next Action loop. The \
         agent claims the task is \
         complete. Review the diff against the task. Respond with ONLY a JSON \
         object: {{\"verdict\": \"approve\"}} or \
         {{\"verdict\": \"issues\", \"issues\": [\"...\", \"...\"]}}. \
         List only real, actionable problems (bugs, task requirements not met, \
         broken code); style nits are NOT issues.\n\n\
         # Task\n{task}\n\n# Completion claim\n{completion}\n\n# Diff\n{diff}"
    )
}

/// 宽松解析：先找 ```json 块，再退回首个 {...} 平衡块；verdict 缺失或
/// 非法 → None（调用方按解析失败降级）。
pub(crate) fn parse_verdict(raw: &str) -> Option<Verdict> {
    let json_str = extract_json(raw)?;
    let v: serde_json::Value = serde_json::from_str(&json_str).ok()?;
    match v.get("verdict")?.as_str()? {
        "approve" => Some(Verdict::Approve),
        "issues" => {
            let issues = v
                .get("issues")
                .and_then(|i| i.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(str::to_string))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if issues.is_empty() {
                Some(Verdict::Approve) // issues 判定但清单为空 = 无事可修
            } else {
                Some(Verdict::Issues(issues))
            }
        }
        _ => None,
    }
}

pub(crate) fn extract_json(raw: &str) -> Option<String> {
    if let Some(start) = raw.find("```json") {
        let rest = &raw[start + 7..];
        if let Some(end) = rest.find("```") {
            return Some(rest[..end].trim().to_string());
        }
    }
    // 首个平衡的 {...}；跟踪字符串与转义，避免 issue 文本里的字面花括号误截。
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

/// 单次审查 LLM 调用（复用 compaction::summarize 同款 ValidatedRequest 通路）。
pub(crate) async fn ask_reviewer(provider: &dyn Provider, prompt: &str) -> Option<Verdict> {
    let msgs = vec![Message {
        role: Role::User,
        content: prompt.to_string(),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    }];
    let validated = match ValidatedRequest::new(&msgs, &[]) {
        Ok(v) => v,
        Err(violations) => {
            warn!(
                "invalid review request ({}); skipping review",
                violations.join("; ")
            );
            return None;
        }
    };
    match provider.generate(validated).await {
        Ok(out) => parse_verdict(&out.content),
        Err(e) => {
            warn!("review model call failed ({e}); skipping review");
            None
        }
    }
}

/// 把 issues 回注为反馈 User 消息文本。
pub(crate) fn render_feedback(issues: &[String]) -> String {
    let list = issues
        .iter()
        .map(|i| format!("- {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "[Pre-completion review] The reviewer found issues that must be fixed \
         before the task can be considered complete:\n{list}\n\
         Fix these, then finish the task."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_approve() {
        assert_eq!(
            parse_verdict(r#"{"verdict":"approve"}"#),
            Some(Verdict::Approve)
        );
    }

    #[test]
    fn parses_fenced_issues() {
        let raw = "Here you go:\n```json\n{\"verdict\":\"issues\",\"issues\":[\"missing test\",\"typo in api\"]}\n```";
        match parse_verdict(raw) {
            Some(Verdict::Issues(v)) => assert_eq!(v, vec!["missing test", "typo in api"]),
            other => panic!("expected issues, got {other:?}"),
        }
    }

    #[test]
    fn issues_verdict_with_empty_list_is_approve() {
        assert_eq!(
            parse_verdict(r#"{"verdict":"issues","issues":[]}"#),
            Some(Verdict::Approve)
        );
    }

    #[test]
    fn garbage_and_unknown_verdict_yield_none() {
        assert_eq!(parse_verdict("not json at all"), None);
        assert_eq!(parse_verdict(r#"{"verdict":"maybe"}"#), None);
        assert_eq!(parse_verdict(r#"{"foo":1}"#), None);
    }

    #[test]
    fn extracts_json_despite_braces_inside_strings() {
        // 回归：issue 文本含字面 { } 不得误截 JSON。
        let raw = r#"note {"verdict":"issues","issues":["fix `impl Foo { bar }` block"]} end"#;
        match parse_verdict(raw) {
            Some(Verdict::Issues(v)) => assert_eq!(v, vec!["fix `impl Foo { bar }` block"]),
            other => panic!("expected issues, got {other:?}"),
        }
    }

    #[test]
    fn extracts_embedded_json_object() {
        let raw = "prefix {\"verdict\":\"approve\"} suffix";
        assert_eq!(parse_verdict(raw), Some(Verdict::Approve));
    }

    #[test]
    fn unified_extract_json_covers_all_former_variants() {
        // M9 统一：memory_distill / reflection / coordinator 的重复实现并入
        // 本函数。此测试锁定统一后单一实现须覆盖全部前身行为——
        // markdown json fence / plain fence / 裸对象 / 字符串内花括号 / 无 JSON。
        let cases: &[(&str, Option<&str>)] = &[
            // 原 coordinator extract_json_block 的 markdown json fence
            (
                "Here's the plan:\n```json\n{\"nodes\":[],\"edges\":[]}\n```\nDone.",
                Some("{\"nodes\":[],\"edges\":[]}"),
            ),
            // 原 coordinator plain fence（无 json 标注）
            (
                "```\n{\"nodes\":[{\"id\":\"x\"}]}\n```",
                Some("{\"nodes\":[{\"id\":\"x\"}]}"),
            ),
            // 原 coordinator 裸对象提取
            (
                " some text {\"key\": \"value\"} trailing ",
                Some("{\"key\": \"value\"}"),
            ),
            // 无平衡花括号 → None（协调器侧走 fallback 计划）
            ("not json at all", None),
        ];
        for (raw, expected) in cases {
            assert_eq!(extract_json(raw).as_deref(), *expected, "input: {raw}");
        }
        // 字符串内字面花括号不得误截（review/蒸馏/反思共用的退化输入）。
        let nested = r#"note {"verdict":"issues","issues":["fix `impl Foo { bar }` block"]} end"#;
        assert_eq!(
            extract_json(nested).as_deref(),
            Some(r#"{"verdict":"issues","issues":["fix `impl Foo { bar }` block"]}"#)
        );
    }

    #[test]
    fn prompt_contains_all_sections_and_feedback_lists_issues() {
        let p = render_review_prompt("fix auth", "done", "diff body");
        for s in ["# Task", "# Completion claim", "# Diff", "verdict"] {
            assert!(p.contains(s), "missing {s}");
        }
        let fb = render_feedback(&["a".into(), "b".into()]);
        assert!(fb.contains("- a") && fb.contains("- b"));
    }

    #[test]
    fn review_prompt_keeps_reflect_phase_and_verdict_contract() {
        let p = render_review_prompt("fix auth", "done", "diff body");
        assert!(p.contains("Reflect phase"));
        assert!(p.contains("{\"verdict\": \"approve\"}"));
        assert!(p.contains("# Task"));
    }

    #[tokio::test]
    async fn collect_diff_returns_none_outside_git_repo() {
        let dir = std::env::temp_dir().join(format!(
            "dnv-b3-nogit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(collect_diff(&dir, 4000).await.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
