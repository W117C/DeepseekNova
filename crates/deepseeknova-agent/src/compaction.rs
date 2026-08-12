//! L3 结构化压缩（B2）：L1/L2 之后仍超阈值时，将可安全驱逐的历史交给
//! （可配置的廉价）模型产出 7 段结构化摘要，经 `Memory::compact` 落回，
//! 并做压缩后状态重建：注入最近改动文件路径清单（仅路径）+ 重放最后一条
//! 用户消息。失败回退 L2-only 现状；连败 3 次本会话熔断。
//!
//! 前缀缓存约束：本模块只改写 volatile 区之后的历史（`Memory` 内容），
//! 绝不触碰 system prefix。

use crate::memory::Memory;
use deepseeknova_context::history::group_into_units;
use deepseeknova_core::{DeepseeknovaError, Message, Role};
use deepseeknova_provider::{Provider, ValidatedRequest};
use tracing::{info, warn};

/// 连败多少次后本会话停用 L3（Claude Code 同款保险）。
const MAX_STRIKES: u32 = 3;

/// 会话级 L3 压缩器：持有熔断状态。
pub(crate) struct L3Compactor {
    failures: u32,
    disabled: bool,
}

impl L3Compactor {
    pub(crate) fn new() -> Self {
        Self {
            failures: 0,
            disabled: false,
        }
    }

    pub(crate) fn is_disabled(&self) -> bool {
        self.disabled
    }

    fn record_failure(&mut self) {
        self.failures += 1;
        if self.failures >= MAX_STRIKES {
            self.disabled = true;
            warn!(
                "L3 compaction disabled for this session after {MAX_STRIKES} consecutive failures"
            );
        }
    }

    fn record_success(&mut self) {
        self.failures = 0;
    }

    /// 尝试一次 L3 压缩。返回 true = 已压缩落回；false = 未压缩
    /// （must_replay 顺延 / 已熔断 / LLM 失败），调用方保持 L2-only 现状。
    pub(crate) async fn try_compact(
        &mut self,
        provider: &dyn Provider,
        memory: &mut Memory,
    ) -> bool {
        if self.disabled {
            return false;
        }
        // 正确性保护：存在未消费的 must_replay 推理块时顺延，不计失败。
        if memory.has_pending_must_replay() {
            info!("L3 deferred: pending must_replay reasoning blocks");
            return false;
        }

        let all_msgs = memory.get_all();
        let last_user = last_user_message(&all_msgs);
        let touched = extract_touched_files(&all_msgs);
        let prompt = render_l3_prompt(&all_msgs);

        // B5：fork 摘要调用携带主线 system 前缀（首条 System 消息），
        // 使摘要请求与主线共享前缀缓存——压缩在独立 Provider 调用中完成
        // （不污染主线上下文），且 system 段命中缓存省 token。
        let system_prefix: Option<&Message> = all_msgs.iter().find(|m| m.role == Role::System);

        match summarize_with_prefix(provider, system_prefix, &prompt).await {
            Ok(digest) => {
                memory.compact(digest, None);
                // 状态重建①：最近改动文件路径清单（仅路径，非内容）。
                if !touched.is_empty() {
                    memory.add_message(Message {
                        role: Role::User,
                        content: format!("[Recently touched files]\n{}", touched.join("\n")),
                        name: None,
                        tool_calls: None,
                        tool_call_id: None,
                        reasoning_content: None,
                        reasoning_signature: None,
                    });
                }
                // 状态重建②（B3）：保留最近 N 个完整 turn（原子单元），而非
                // 仅重放最后一条用户消息——Codex 模式"最近消息 + 摘要"：
                // 最近 turn 对继续任务价值最高，按 `group_into_units` 原子保留
                // （assistant(tool_calls)→Tool 结果配对不破坏 replay 不变量）。
                for unit in recent_units(&all_msgs, KEEP_RECENT_UNITS) {
                    match unit {
                        deepseeknova_context::history::HistoryUnit::Standalone(msg) => {
                            memory.add_message(msg.clone());
                        }
                        deepseeknova_context::history::HistoryUnit::ToolExchange {
                            assistant,
                            results,
                        } => {
                            memory.add_message(assistant.clone());
                            for r in results {
                                memory.add_message(r.clone());
                            }
                        }
                    }
                }
                // 状态重建③：若最近 turn 未覆盖最后用户消息（如最后一条是
                // 纯文本 assistant），补重放用户意图，让任务从原意图继续。
                let rebuilt = memory.get_all();
                if let Some(u) = last_user {
                    if !rebuilt.iter().any(|m| m.content == u.content) {
                        memory.add_message(u);
                    }
                }
                self.record_success();
                true
            }
            Err(e) => {
                warn!("L3 compaction failed ({e}); falling back to L2-only");
                self.record_failure();
                false
            }
        }
    }
}

/// B3：压缩后保留的最近完整 turn 数（原子单元计数）。
const KEEP_RECENT_UNITS: usize = 3;

/// 取历史末尾最近 `n` 个压缩安全单元（按原顺序返回）。
fn recent_units(messages: &[Message], n: usize) -> Vec<deepseeknova_context::history::HistoryUnit> {
    let units = group_into_units(messages);
    units
        .into_iter()
        .rev()
        .take(n)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// 渲染 7 段结构化摘要 prompt（要求直引原文关键短语防漂移）。
fn render_l3_prompt(messages: &[Message]) -> String {
    // 按压缩安全单元渲染，tool 交换以紧凑形式呈现。
    let units = group_into_units(messages);
    let mut convo = String::new();
    for u in &units {
        convo.push_str(&render_unit(u));
        convo.push('\n');
    }
    format!(
        "You are the memory stage of the Observe → Plan → Tool → Verify → \
         Reflect → Next Action loop, compacting an agent conversation into a \
         structured digest. \
         Produce EXACTLY these seven sections, each as a markdown heading, \
         quoting key phrases verbatim from the source to avoid drift:\n\
         ## Original intent\n## Key decisions\n## Files involved\n\
         ## Errors & fixes\n## TODOs\n## In progress\n## Next step\n\n\
         Conversation:\n{convo}"
    )
}

fn render_unit(u: &deepseeknova_context::history::HistoryUnit) -> String {
    use deepseeknova_context::history::HistoryUnit;
    match u {
        HistoryUnit::Standalone(m) => format!("[{:?}] {}", m.role, m.content),
        HistoryUnit::ToolExchange { assistant, results } => {
            let calls: Vec<String> = assistant
                .tool_calls
                .iter()
                .flatten()
                .map(|tc| format!("{}({})", tc.function.name, tc.function.arguments))
                .collect();
            let outs: Vec<String> = results.iter().map(|r| truncate(&r.content, 400)).collect();
            format!(
                "[ToolExchange] calls: {} | results: {}",
                calls.join("; "),
                outs.join(" | ")
            )
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}…")
    }
}

/// 从被压缩历史的 write/edit 类工具调用参数中尽力提取 `path` 字段。
/// 解析失败一律静默跳过——这是提示性重建，不是事实源。
fn extract_touched_files(messages: &[Message]) -> Vec<String> {
    const WRITE_TOOLS: [&str; 4] = ["write_file", "edit_file", "apply_patch", "create_file"];
    let mut seen = std::collections::BTreeSet::new();
    for m in messages {
        for tc in m.tool_calls.iter().flatten() {
            if !WRITE_TOOLS.contains(&tc.function.name.as_str()) {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&tc.function.arguments) {
                if let Some(p) = v.get("path").and_then(|p| p.as_str()) {
                    seen.insert(p.to_string());
                }
            }
        }
    }
    seen.into_iter().collect()
}

pub(crate) fn last_user_message(messages: &[Message]) -> Option<Message> {
    messages
        .iter()
        .rev()
        // 跳过合成召回消息（<recalled-memory>），避免把检索块当用户意图。
        .find(|m| m.role == Role::User && !m.content.starts_with("<recalled-memory>"))
        .cloned()
}

/// 单次 LLM 摘要调用：走 Provider 非流式生成，取文本。
///
/// B5：`system_prefix` 为主线首条 System 消息（若有）——作为请求首条消息
/// 携带，使摘要调用与主线共享前缀缓存（fork 调用不污染主线上下文）。
async fn summarize_with_prefix(
    provider: &dyn Provider,
    system_prefix: Option<&Message>,
    prompt: &str,
) -> Result<String, DeepseeknovaError> {
    let mut msgs: Vec<Message> = Vec::new();
    if let Some(sys) = system_prefix {
        msgs.push(sys.clone());
    }
    msgs.push(Message {
        role: Role::User,
        content: prompt.to_string(),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        reasoning_signature: None,
    });
    let validated = ValidatedRequest::new(&msgs, &[]).map_err(|violations| {
        DeepseeknovaError::runner(format!(
            "invalid compact request: {}",
            violations.join("; ")
        ))
    })?;
    let out = provider.generate(validated).await?;
    if out.content.trim().is_empty() {
        return Err(DeepseeknovaError::runner(
            "empty digest from compact model".to_string(),
        ));
    }
    Ok(out.content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_core::types::{FunctionCall, ToolCall};

    fn msg(role: Role, content: &str) -> Message {
        Message {
            role,
            content: content.into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
        }
    }

    #[test]
    fn prompt_renders_seven_sections() {
        let p = render_l3_prompt(&[msg(Role::User, "fix the bug in auth")]);
        for h in [
            "## Original intent",
            "## Key decisions",
            "## Files involved",
            "## Errors & fixes",
            "## TODOs",
            "## In progress",
            "## Next step",
        ] {
            assert!(p.contains(h), "missing section {h}");
        }
        assert!(p.contains("fix the bug in auth"));
    }

    #[test]
    fn compaction_prompt_identifies_as_loop_memory_stage() {
        let p = render_l3_prompt(&[msg(Role::User, "fix the bug in auth")]);
        assert!(p.contains("Observe → Plan → Tool → Verify → Reflect → Next Action"));
        assert!(p.contains("## Next step"));
    }

    #[test]
    fn extracts_paths_only_from_write_tools() {
        let mut m = msg(Role::Assistant, "");
        m.tool_calls = Some(vec![
            ToolCall {
                id: "1".into(),
                ty: "function".into(),
                function: FunctionCall {
                    name: "write_file".into(),
                    arguments: "{\"path\":\"src/a.rs\",\"content\":\"x\"}".into(),
                },
            },
            ToolCall {
                id: "2".into(),
                ty: "function".into(),
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: "{\"path\":\"src/b.rs\"}".into(),
                },
            },
            ToolCall {
                id: "3".into(),
                ty: "function".into(),
                function: FunctionCall {
                    name: "edit_file".into(),
                    arguments: "not-json".into(),
                },
            },
        ]);
        let files = extract_touched_files(&[m]);
        assert_eq!(files, vec!["src/a.rs".to_string()]); // read 不算、坏 JSON  跳过
    }

    #[test]
    fn strike_counter_disables_after_three() {
        let mut c = L3Compactor::new();
        assert!(!c.is_disabled());
        c.record_failure();
        c.record_failure();
        assert!(!c.is_disabled());
        c.record_failure();
        assert!(c.is_disabled());
    }

    #[test]
    fn success_resets_strikes() {
        let mut c = L3Compactor::new();
        c.record_failure();
        c.record_failure();
        c.record_success();
        c.record_failure();
        assert!(!c.is_disabled(), "success must reset the strike counter");
    }

    #[test]
    fn last_user_message_picks_most_recent() {
        let msgs = vec![
            msg(Role::User, "first"),
            msg(Role::Assistant, "reply"),
            msg(Role::User, "second"),
        ];
        assert_eq!(last_user_message(&msgs).unwrap().content, "second");
    }
}
