//! # 结构化输出契约（Structured Output Contracts）
//!
//! 让所有 LLM 判定点（plan / reflect / verify / review / 蒸馏）的输出通过
//! 同一条**确定性**流水线：宽松提取 → 校验 → 失败回显重试 → 回退默认。
//! 这是「提示词无关任务能力」的核心：任务质量由 harness 的契约机制承担，
//! 不依赖模型对提示词格式的遵循能力——弱模型也能产出可用结构化结果。
//!
//! 流水线语义：
//! 1. [`crate::contract::extract_json`]：宽松提取 JSON（```json 围栏 / 纯围栏 /
//!    首个平衡 `{}`，字符串与转义感知，不误截字面花括号）；
//! 2. 调用方解析器（如 `parse_verdict`）校验结构与字段；
//! 3. 失败时 [`crate::contract::retry_parsed`] 构造「上次输出 + 格式错误回显」
//!    的修正提示，在 `max_retries` 上限内重试；
//! 4. 重试耗尽仍未通过 → 调用方按 [`crate::contract::ContractOutcome::Fallback`]
//!    回退默认（不无限重试烧 token）。
//!
//! 各判定点迁移后共享本模块的提取与重试逻辑，消除 `review::extract_json`
//! 被 coordinator/verify/reflection/distill 四处复制的历史形态。

use serde_json::Value;

/// 宽松 JSON 提取：先找 ```json 围栏，再退回首个平衡的 `{...}` 块。
///
/// 平衡扫描对字符串与转义感知，避免 issue 文本中的字面花括号（如
/// `impl Foo { bar }`）被误截。这是全仓唯一的宽松提取实现。
pub(crate) fn extract_json(raw: &str) -> Option<String> {
    if let Some(start) = raw.find("```json") {
        let rest = &raw[start + 7..];
        if let Some(end) = rest.find("```") {
            return Some(rest[..end].trim().to_string());
        }
    }
    // 首个平衡的 {...}；跟踪字符串与转义，避免 issue 文本里的字面花括号
    // （如 `impl Foo { bar }`）误截。纯 ``` 围栏内容若无 `{` 也会由此
    // 分支得到 None（与原 review::extract_json 行为逐字节一致）。
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

/// 一次契约调用的最终结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ContractOutcome<T> {
    /// 校验通过（可能发生在首次或某次重试）。
    Ok(T),
    /// 全部重试仍失败，调用方回退默认值。
    Fallback,
    /// 调用层本身失败（provider 错误 / 无输出），无重试价值。
    CallError(String),
}

impl<T> ContractOutcome<T> {
    /// 取成功值（`Fallback` / `CallError` 为 None）。测试辅助：生产路径
    /// 用 `match` 显式处理三态，不依赖此便利方法。
    #[cfg(test)]
    pub(crate) fn ok(self) -> Option<T> {
        match self {
            Self::Ok(v) => Some(v),
            _ => None,
        }
    }
}

/// 在 `max_retries` 上限内重复调用 `call`，直到 `parse` 成功。
///
/// `prompt` 构造每次调用的提示：首次收到 `(None, None)` 应返回初始提示；
/// 重试收到 `(Some(last_raw), Some(reason))` 应返回「初始提示 + 上次输出 +
/// 失败原因」的修正提示（失败原因来自 `parse` 返回的 `Err`）。`call`
/// 接收构造好的提示、返回 LLM 原始文本；返回 `None` 视为调用层失败
/// （`CallError`）。`parse` 校验并提取目标类型，`Err(reason)` 触发回显重试。
/// 重试耗尽仍未通过 → `Fallback`（调用方回退默认，不无限重试烧 token）。
pub(crate) async fn retry_parsed<T, F, Fut, P>(
    max_retries: u32,
    mut call: F,
    parse: impl Fn(&str) -> Result<T, String>,
    mut prompt: P,
) -> ContractOutcome<T>
where
    F: FnMut(&str) -> Fut,
    Fut: std::future::Future<Output = Option<String>>,
    P: FnMut(Option<&str>, Option<&str>) -> String,
{
    let mut last_raw: Option<String> = None;
    let mut last_reason: Option<String> = None;
    for attempt in 0..=max_retries {
        let p = prompt(last_raw.as_deref(), last_reason.as_deref());
        let raw = match call(&p).await {
            Some(r) => r,
            None => return ContractOutcome::CallError("no output from model".into()),
        };
        match parse(&raw) {
            Ok(parsed) => return ContractOutcome::Ok(parsed),
            Err(reason) => {
                if attempt == max_retries {
                    return ContractOutcome::Fallback;
                }
                tracing::warn!(
                    attempt = attempt + 1,
                    max_retries,
                    reason = %reason,
                    "structured output failed validation, retrying with echo"
                );
                last_raw = Some(raw);
                last_reason = Some(reason);
            }
        }
    }
    ContractOutcome::Fallback
}

/// 将上次输出与格式错误拼成修正提示的默认策略（中文，供调用方复用）。
/// 用法：`|last, reason| default_echo(initial_prompt, last, reason)`。
pub(crate) fn default_echo(initial_prompt: &str, last_raw: &str, hint: &str) -> String {
    format!(
        "{initial_prompt}\n\n---\n\
         你的上一次输出不符合要求的 JSON 格式（{hint}）。\n\
         原始输出：\n{last_raw}\n\
         请重新输出，只输出符合要求的 JSON 对象，不要附加解释。"
    )
}

/// 从 Value 中校验必需字符串字段；缺失或类型不符返回错误说明。
pub(crate) fn require_string<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing/invalid string field `{key}`"))
}

/// 从 Value 中校验必需布尔字段；缺失或类型不符返回错误说明。
pub(crate) fn require_bool(v: &Value, key: &str) -> Result<bool, String> {
    v.get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("missing/invalid boolean field `{key}`"))
}

/// 保留字符串**尾部**至多 `max_chars` 字符（预算裁剪；CJK 安全，按字符
/// 而非字节切分）。供召回注入等 volatile 内容预算化使用——截断保留
/// 最新内容（记忆/结果的近期信息价值更高）。
pub(crate) fn truncate_front(s: String, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s;
    }
    let tail: String = s
        .chars()
        .rev()
        .take(max_chars)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…[truncated, {max_chars} chars kept]…\n{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_fenced_json_block() {
        assert_eq!(
            extract_json("Here's the plan:\n```json\n{\"nodes\":[]}\n```\nDone.").as_deref(),
            Some("{\"nodes\":[]}")
        );
    }

    #[test]
    fn extract_json_plain_fence() {
        assert_eq!(
            extract_json("```\n{\"nodes\":[{\"id\":\"x\"}]}\n```").as_deref(),
            Some("{\"nodes\":[{\"id\":\"x\"}]}")
        );
    }

    #[test]
    fn extract_json_bare_object() {
        assert_eq!(
            extract_json(" some text {\"key\": \"value\"} trailing ").as_deref(),
            Some("{\"key\": \"value\"}")
        );
    }

    #[test]
    fn extract_json_none_when_no_braces() {
        assert_eq!(extract_json("not json at all"), None);
    }

    #[test]
    fn extract_json_ignores_literal_braces_in_strings() {
        // 字符串内字面花括号不得误截（review/蒸馏/反思共用的退化输入）。
        let nested = r#"note {"verdict":"issues","issues":["fix `impl Foo { bar }` block"]} end"#;
        assert_eq!(
            extract_json(nested).as_deref(),
            Some(r#"{"verdict":"issues","issues":["fix `impl Foo { bar }` block"]}"#)
        );
    }

    #[tokio::test]
    async fn retry_parsed_succeeds_on_first_call() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let calls2 = calls.clone();
        let out = retry_parsed(
            2,
            |_prompt| {
                calls2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { Some("{\"ok\": true}".to_string()) }
            },
            |raw| {
                serde_json::from_str::<Value>(raw)
                    .ok()
                    .and_then(|v| v.get("ok").and_then(Value::as_bool))
                    .ok_or_else(|| "missing `ok`".to_string())
            },
            |_last, _reason| "initial".to_string(),
        )
        .await;
        assert_eq!(out, ContractOutcome::Ok(true));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retry_parsed_retries_then_falls_back() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let calls2 = calls.clone();
        let echoed = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let echoed2 = echoed.clone();
        let out = retry_parsed(
            2,
            |_prompt| {
                calls2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { Some("not json at all".to_string()) }
            },
            |raw| serde_json::from_str::<Value>(raw).map_err(|e| e.to_string()),
            |last, reason| {
                if last.is_some() && reason.is_some() {
                    echoed2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                "prompt".to_string()
            },
        )
        .await;
        assert_eq!(out, ContractOutcome::Fallback);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
        assert_eq!(
            echoed.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "两次重试都应携带上次输出与原因"
        );
    }

    #[tokio::test]
    async fn retry_parsed_call_error_propagates() {
        let out = retry_parsed(
            2,
            |_prompt| async { None::<String> },
            |_raw| Ok(true),
            |_last, _reason| "initial".to_string(),
        )
        .await;
        assert!(matches!(out, ContractOutcome::CallError(_)));
    }

    #[test]
    fn default_echo_includes_initial_prompt_original_and_hint() {
        let e = default_echo("initial prompt", "garbage", "missing `verdict`");
        assert!(e.contains("initial prompt"));
        assert!(e.contains("garbage"));
        assert!(e.contains("missing `verdict`"));
        assert!(e.contains("只输出符合要求的 JSON"));
    }

    #[test]
    fn require_string_and_bool_validators() {
        let v: Value = serde_json::json!({"name": "x", "flag": true});
        assert_eq!(require_string(&v, "name"), Ok("x"));
        assert!(require_string(&v, "missing").is_err());
        assert_eq!(require_bool(&v, "flag"), Ok(true));
        assert!(require_bool(&v, "name").is_err());
    }

    #[test]
    fn contract_outcome_ok_extracts_value() {
        assert_eq!(ContractOutcome::Ok(5u32).ok(), Some(5));
        assert_eq!(ContractOutcome::<u32>::Fallback.ok(), None);
        assert_eq!(ContractOutcome::<u32>::CallError("e".into()).ok(), None);
    }
}
