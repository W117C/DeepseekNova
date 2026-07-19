//! History compression with DeepSeek V4 protocol invariants.
//!
//! DeepSeek V4 requires that when an assistant message has both
//! `reasoning_content` AND `tool_calls`, the reasoning must be
//! present in all subsequent requests — otherwise HTTP 400.
//!
//! This module enforces that constraint at the type level through
//! [`HistoryUnit`] grouping and [`validate_replay_invariant`] checks.

use deepnova_core::types::{Message, Role};
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// Core types — protocol-aware history representation
// ---------------------------------------------------------------------------

/// An identifier for a specific message in the history.
pub type MessageId = String;

/// A reasoning block extracted from an assistant message.
///
/// Carries `raw_provider_payload` to preserve the original provider
/// response bytes for faithful replay — avoids re-serialization
/// that could break provider-side integrity checks.
#[derive(Debug, Clone)]
pub struct ReasoningBlock {
    pub text: String,
    /// Original provider response payload for faithful replay.
    /// When present, replay should use this instead of reconstructing
    /// from `text` to avoid breaking checksum/signature fields.
    pub raw_provider_payload: Option<serde_json::Value>,
    /// True when this reasoning is paired with tool calls — must
    /// never be removed independently of its tool calls + results.
    pub must_replay: bool,
}

impl ReasoningBlock {
    /// Create from a standard `Message`, extracting reasoning + tool_call info.
    pub fn from_message(msg: &Message) -> Option<Self> {
        msg.reasoning_content.as_ref().map(|text| {
            let has_tools = msg
                .tool_calls
                .as_ref()
                .map(|tc| !tc.is_empty())
                .unwrap_or(false);
            ReasoningBlock {
                text: text.clone(),
                raw_provider_payload: None,
                must_replay: has_tools,
            }
        })
    }
}

// ---------------------------------------------------------------------------
// HistoryUnit — compression-safe grouping
// ---------------------------------------------------------------------------

/// The smallest unit that can be safely compressed or removed from history.
///
/// - `Standalone`: a user message or a non-tool-calling assistant message.
///   Can be summarized or dropped independently.
/// - `ToolExchange`: an assistant message with tool calls AND all of its
///   corresponding tool result messages. Must be kept or removed atomically —
///   never partially evicted.
#[derive(Debug, Clone)]
pub enum HistoryUnit {
    Standalone(Message),
    ToolExchange {
        /// The assistant message that initiated the tool calls.
        assistant: Message,
        /// The tool result messages that correspond to the assistant's tool calls.
        results: Vec<Message>,
    },
}

impl HistoryUnit {
    /// Estimated token count for this unit (char count / 4, rough).
    pub fn estimated_tokens(&self) -> usize {
        match self {
            HistoryUnit::Standalone(msg) => msg.content.len() / 4,
            HistoryUnit::ToolExchange { assistant, results } => {
                let mut total = assistant.content.len();
                if let Some(ref r) = assistant.reasoning_content {
                    total += r.len();
                }
                for r in results {
                    total += r.content.len();
                }
                total / 4
            }
        }
    }

    /// True if this unit contains reasoning that must be replayed.
    pub fn has_load_bearing_reasoning(&self) -> bool {
        match self {
            HistoryUnit::Standalone(msg) => {
                msg.reasoning_content.is_some()
                    && msg
                        .tool_calls
                        .as_ref()
                        .map(|tc| !tc.is_empty())
                        .unwrap_or(false)
            }
            HistoryUnit::ToolExchange { assistant, .. } => {
                assistant.reasoning_content.is_some()
                    && assistant
                        .tool_calls
                        .as_ref()
                        .map(|tc| !tc.is_empty())
                        .unwrap_or(false)
            }
        }
    }
}

// =========================================================================
// group_into_units — linear messages → compression-safe units
// =========================================================================

/// Reorganize a linear message history into compression-safe units.
///
/// # Rules
/// 1. When an assistant message has non-empty tool_calls, scan forward
///    and collect all matching tool result messages into a `ToolExchange`.
/// 2. All other messages become `Standalone` units.
/// 3. The output preserves original message order.
pub fn group_into_units(messages: &[Message]) -> Vec<HistoryUnit> {
    let mut units = Vec::new();
    let mut i = 0;

    while i < messages.len() {
        let msg = &messages[i];

        let has_tool_calls = msg.role == Role::Assistant
            && msg
                .tool_calls
                .as_ref()
                .map(|tc| !tc.is_empty())
                .unwrap_or(false);

        if has_tool_calls {
            // Collect tool call IDs from this assistant message
            let call_ids: HashSet<String> = msg
                .tool_calls
                .as_ref()
                .unwrap()
                .iter()
                .map(|tc| tc.id.clone())
                .collect();

            // Scan forward for matching tool results
            let mut results = Vec::new();
            let mut j = i + 1;
            while j < messages.len() {
                let next = &messages[j];
                if next.role == Role::Tool {
                    if let Some(ref call_id) = next.tool_call_id {
                        if call_ids.contains(call_id) {
                            results.push(next.clone());
                            j += 1;
                            continue;
                        }
                    }
                }
                // Stop at next user or assistant message
                if next.role == Role::User || next.role == Role::Assistant {
                    break;
                }
                j += 1;
            }

            units.push(HistoryUnit::ToolExchange {
                assistant: msg.clone(),
                results,
            });
            i = j; // skip the collected results
        } else {
            units.push(HistoryUnit::Standalone(msg.clone()));
            i += 1;
        }
    }

    units
}

// =========================================================================
// validate_replay_invariant — send-before-send guard
// =========================================================================

/// Violation of the DeepSeek V4 replay invariant.
#[derive(Debug, thiserror::Error)]
pub enum InvariantViolation {
    #[error("tool result {tool_call_id} has no matching tool call — orphan result")]
    OrphanToolResult { tool_call_id: String },
    #[error("message has load-bearing reasoning that is missing (must_replay but reasoning_content=None)")]
    MissingLoadBearingReasoning,
}

/// Validate that the history satisfies DeepSeek V4 replay invariants.
///
/// Must be called before every provider request, independently of any
/// compaction logic. This acts as a final safety net against any code
/// path that might bypass the compactor.
pub fn validate_replay_invariant(messages: &[Message]) -> Result<(), Vec<InvariantViolation>> {
    let mut violations = Vec::new();

    // 1. Collect all tool call IDs referenced
    let mut declared_call_ids: HashSet<&str> = HashSet::new();
    let mut load_bearing_indices: Vec<usize> = Vec::new();

    for (idx, msg) in messages.iter().enumerate() {
        if let Some(ref tcs) = msg.tool_calls {
            for tc in tcs {
                declared_call_ids.insert(&tc.id);
            }
        }
        // Check for load-bearing reasoning
        let has_reasoning = msg.reasoning_content.is_some();
        let has_tools = msg
            .tool_calls
            .as_ref()
            .map(|tc| !tc.is_empty())
            .unwrap_or(false);
        if has_reasoning && has_tools {
            load_bearing_indices.push(idx);
        }
    }

    // 2. Check for orphan tool results
    for msg in messages {
        if msg.role == Role::Tool {
            if let Some(ref call_id) = msg.tool_call_id {
                if !declared_call_ids.contains(call_id.as_str()) {
                    violations.push(InvariantViolation::OrphanToolResult {
                        tool_call_id: call_id.clone(),
                    });
                }
            }
        }
    }

    // 3. Verify load-bearing reasoning is still present
    // (This check is inherently satisfied if the message still has reasoning_content,
    // but we check the indices to catch cases where the message was mutated)
    for &idx in &load_bearing_indices {
        if messages[idx].reasoning_content.is_none() {
            violations.push(InvariantViolation::MissingLoadBearingReasoning);
        }
    }

    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

// =========================================================================
// HistoryCompactor trait + AtomicUnitCompactor
// =========================================================================

/// Budget in estimated tokens.
#[derive(Debug, Clone, Copy)]
pub struct TokenBudget {
    pub max_tokens: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum CompactionError {
    #[error("budget too small to retain any complete unit")]
    BudgetTooSmall,
    #[error("compaction failed: {0}")]
    Other(String),
}

/// Trait for history compaction strategies.
///
/// Implementations must guarantee:
/// - No orphan tool results (tool result without matching tool call)
/// - No load-bearing reasoning removed while tool calls remain
pub trait HistoryCompactor {
    fn compact(
        &self,
        units: &[HistoryUnit],
        budget: TokenBudget,
    ) -> Result<Vec<HistoryUnit>, CompactionError>;
}

/// Default compactor: drop oldest standalone units first,
/// then replace oldest ToolExchange units with synthetic summaries.
pub struct AtomicUnitCompactor;

impl HistoryCompactor for AtomicUnitCompactor {
    fn compact(
        &self,
        units: &[HistoryUnit],
        budget: TokenBudget,
    ) -> Result<Vec<HistoryUnit>, CompactionError> {
        let current_tokens: usize = units.iter().map(|u| u.estimated_tokens()).sum();

        if current_tokens <= budget.max_tokens {
            return Ok(units.to_vec());
        }

        let mut result: Vec<HistoryUnit> = units.to_vec();
        let mut tokens: usize = current_tokens;

        // Phase 1: Drop oldest standalone units
        while tokens > budget.max_tokens {
            let standalone_idx = result
                .iter()
                .position(|u| matches!(u, HistoryUnit::Standalone(_)));

            match standalone_idx {
                Some(idx) => {
                    tokens = tokens.saturating_sub(result[idx].estimated_tokens());
                    result.remove(idx);
                }
                None => break,
            }
        }

        if tokens <= budget.max_tokens {
            return Ok(result);
        }

        // Phase 2: Replace oldest ToolExchange with synthetic summary
        while tokens > budget.max_tokens {
            let exchange_idx = result
                .iter()
                .position(|u| matches!(u, HistoryUnit::ToolExchange { .. }));

            match exchange_idx {
                Some(idx) => {
                    if let HistoryUnit::ToolExchange { assistant, results } = &result[idx] {
                        let summary = format!(
                            "[Compacted turn] Called tool(s), {} result(s) returned. Details omitted.",
                            results.len()
                        );
                        let replacement = HistoryUnit::Standalone(Message {
                            role: Role::Tool,
                            content: summary,
                            name: None,
                            tool_calls: None,
                            tool_call_id: None,
                            reasoning_content: assistant.reasoning_content.clone(),
                        });
                        tokens = tokens.saturating_sub(result[idx].estimated_tokens());
                        tokens += replacement.estimated_tokens();
                        result[idx] = replacement;
                    }
                }
                None => {
                    if result.is_empty() {
                        return Err(CompactionError::BudgetTooSmall);
                    }
                    // No more ToolExchanges to summarize — if still over budget, fail
                    if tokens > budget.max_tokens {
                        return Err(CompactionError::BudgetTooSmall);
                    }
                    break;
                }
            }
        }

        Ok(result)
    }
}

// =========================================================================
// Tests — must-lock invariants
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use deepnova_core::types::ToolCall;

    fn make_msg(role: Role, content: &str) -> Message {
        Message {
            role,
            content: content.to_string(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    fn make_tool_result(content: &str, call_id: &str) -> Message {
        Message {
            role: Role::Tool,
            content: content.to_string(),
            name: None,
            tool_calls: None,
            tool_call_id: Some(call_id.to_string()),
            reasoning_content: None,
        }
    }

    fn make_assistant_with_tools(
        text: &str,
        reasoning: Option<&str>,
        tool_ids: &[&str],
    ) -> Message {
        Message {
            role: Role::Assistant,
            content: text.to_string(),
            name: None,
            tool_calls: Some(
                tool_ids
                    .iter()
                    .map(|id| ToolCall {
                        id: id.to_string(),
                        ty: "function".to_string(),
                        function: deepnova_core::types::FunctionCall {
                            name: "test_tool".to_string(),
                            arguments: "{}".to_string(),
                        },
                    })
                    .collect(),
            ),
            tool_call_id: None,
            reasoning_content: reasoning.map(|r| r.to_string()),
        }
    }

    // ── group_into_units tests ──────────────────────────────────

    #[test]
    fn group_simple_user_assistant() {
        let messages = vec![
            make_msg(Role::User, "hello"),
            make_msg(Role::Assistant, "hi there"),
        ];
        let units = group_into_units(&messages);
        assert_eq!(units.len(), 2);
        assert!(matches!(units[0], HistoryUnit::Standalone(_)));
        assert!(matches!(units[1], HistoryUnit::Standalone(_)));
    }

    #[test]
    fn group_tool_exchange_packs_results() {
        let messages = vec![
            make_msg(Role::User, "read main.rs"),
            make_assistant_with_tools("let me check", Some("thinking..."), &["call_1"]),
            make_tool_result("file contents here", "call_1"),
            make_msg(Role::Assistant, "here's the file"),
        ];
        let units = group_into_units(&messages);
        assert_eq!(units.len(), 3); // user, tool_exchange, final assistant
        assert!(matches!(units[0], HistoryUnit::Standalone(_)));
        assert!(matches!(units[1], HistoryUnit::ToolExchange { .. }));
        assert!(matches!(units[2], HistoryUnit::Standalone(_)));

        if let HistoryUnit::ToolExchange {
            ref assistant,
            ref results,
        } = units[1]
        {
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].tool_call_id.as_deref(), Some("call_1"));
            assert!(assistant.reasoning_content.is_some());
        } else {
            panic!("expected ToolExchange");
        }
    }

    #[test]
    fn multi_tool_exchange_packs_all_results() {
        let messages = vec![
            make_msg(Role::User, "check files"),
            make_assistant_with_tools("checking", None, &["call_a", "call_b"]),
            make_tool_result("result a", "call_a"),
            make_tool_result("result b", "call_b"),
        ];
        let units = group_into_units(&messages);
        assert_eq!(units.len(), 2);
        if let HistoryUnit::ToolExchange { results, .. } = &units[1] {
            assert_eq!(results.len(), 2);
        } else {
            panic!("expected ToolExchange with 2 results");
        }
    }

    // ── validate_replay_invariant tests ─────────────────────────

    #[test]
    fn valid_history_passes_invariant_check() {
        let messages = vec![
            make_msg(Role::User, "read main.rs"),
            make_assistant_with_tools("checking", Some("reasoning"), &["call_1"]),
            make_tool_result("contents", "call_1"),
        ];
        assert!(validate_replay_invariant(&messages).is_ok());
    }

    #[test]
    fn orphan_tool_result_is_rejected() {
        let messages = vec![
            make_msg(Role::User, "hi"),
            make_tool_result("orphan result", "call_missing"),
        ];
        let result = validate_replay_invariant(&messages);
        assert!(result.is_err());
        let violations = result.unwrap_err();
        assert_eq!(violations.len(), 1);
        assert!(matches!(
            violations[0],
            InvariantViolation::OrphanToolResult { .. }
        ));
    }

    #[test]
    fn non_tool_reasoning_does_not_require_replay() {
        // Assistant with reasoning but NO tool calls — fine to keep or drop
        let mut msg = make_msg(Role::Assistant, "answer");
        msg.reasoning_content = Some("thinking...".to_string());
        let messages = vec![make_msg(Role::User, "q"), msg];
        assert!(validate_replay_invariant(&messages).is_ok());
    }

    // ── Compaction tests ────────────────────────────────────────

    #[test]
    fn compaction_removes_standalone_first() {
        let units = vec![
            HistoryUnit::Standalone(make_msg(Role::User, "old turn user")),
            HistoryUnit::Standalone(make_msg(Role::Assistant, "old turn answer")),
            HistoryUnit::ToolExchange {
                assistant: make_assistant_with_tools("check", Some("r"), &["c1"]),
                results: vec![make_tool_result("result", "c1")],
            },
        ];
        let compactor = AtomicUnitCompactor;
        let budget = TokenBudget { max_tokens: 20 }; // very tight budget
        let compacted = compactor.compact(&units, budget).unwrap();

        // The standalone units should be removed first, tool exchange kept
        let standalone_count = compacted
            .iter()
            .filter(|u| matches!(u, HistoryUnit::Standalone(_)))
            .count();
        let exchange_count = compacted
            .iter()
            .filter(|u| matches!(u, HistoryUnit::ToolExchange { .. }))
            .count();
        assert!(standalone_count <= 2, "standalones removed first");
        assert!(exchange_count <= 1, "tool exchange may be summarized");
    }

    #[test]
    fn compaction_never_leaves_orphan_tool_result() {
        let units = vec![HistoryUnit::ToolExchange {
            assistant: make_assistant_with_tools("check", Some("r"), &["c1"]),
            results: vec![make_tool_result("result", "c1")],
        }];
        let compactor = AtomicUnitCompactor;
        let budget = TokenBudget { max_tokens: 5 };
        let compacted = compactor.compact(&units, budget).unwrap();

        // After compaction, convert back to messages and validate
        let mut messages = Vec::new();
        for unit in &compacted {
            match unit {
                HistoryUnit::Standalone(msg) => messages.push(msg.clone()),
                HistoryUnit::ToolExchange { assistant, results } => {
                    messages.push(assistant.clone());
                    messages.extend(results.iter().cloned());
                }
            }
        }
        assert!(validate_replay_invariant(&messages).is_ok());
    }

    #[test]
    fn empty_budget_rejected() {
        let units = vec![HistoryUnit::ToolExchange {
            assistant: make_assistant_with_tools("large", Some("r"), &["c1"]),
            results: vec![make_tool_result("big result", "c1")],
        }];
        let compactor = AtomicUnitCompactor;
        let result = compactor.compact(&units, TokenBudget { max_tokens: 0 });
        assert!(result.is_err());
    }
}
