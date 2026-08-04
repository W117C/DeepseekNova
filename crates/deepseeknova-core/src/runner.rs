use crate::chunk::Usage;
use crate::protocol::{DriftFinding, GateViolation, PhaseTransition};
use crate::tool_hook::QualityFinding;
use crate::types::ToolCall;
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use tokio_stream::StreamExt;

/// Runner is the execution abstraction. Agent, Planner, Coordinator,
/// SubAgent, and ServerRunner all implement it. Runtime doesn't
/// discriminate between them.
#[async_trait::async_trait]
pub trait Runner: Send + Sync {
    /// Streaming run — returns a stream of events.
    async fn run_stream(&self, input: RunInput) -> anyhow::Result<RunEventStream>;

    /// Convenience: collect the stream into a final output.
    async fn run(&self, input: RunInput) -> anyhow::Result<RunOutput> {
        let mut stream = self.run_stream(input).await?;
        let mut text = String::new();
        let mut tool_calls = Vec::new();
        let mut usage = None;
        while let Some(event) = stream.next().await {
            match event? {
                RunEvent::TextDelta(delta) => text.push_str(&delta),
                RunEvent::ToolCallEnd {
                    id,
                    name,
                    arguments,
                } => {
                    tool_calls.push(ToolCall {
                        id,
                        ty: "function".to_string(),
                        function: crate::types::FunctionCall { name, arguments },
                    });
                }
                RunEvent::Usage(u) => usage = Some(u),
                RunEvent::Done(output) => return Ok(output),
                _ => {}
            }
        }
        Ok(RunOutput {
            text,
            tool_calls,
            usage,
        })
    }
}

#[derive(Debug, Clone)]
pub struct RunInput {
    pub prompt: String,
    pub images: Vec<String>, // data: URLs for vision-capable models
    pub model_override: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RunOutput {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Option<Usage>,
}

pub type RunEventStream = Pin<Box<dyn Stream<Item = anyhow::Result<RunEvent>> + Send>>;

/// Resolves a permission-gate `Ask` decision by asking a frontend for a user
/// decision. Returning `true` allows the pending tool call; `false` denies it.
///
/// Frontends that can prompt a user (desktop app, HTTP server) implement this.
/// When no responder is attached, the agent falls back to allowing `Ask`
/// decisions so non-interactive callers (CLI, tests) keep working.
#[async_trait::async_trait]
pub trait ApprovalResponder: Send + Sync {
    async fn request(&self, id: &str, title: &str, description: Option<&str>) -> bool;
}

/// RunEvent has no Error variant — errors ride the Stream's Result.
#[derive(Debug, Clone)]
pub enum RunEvent {
    TextDelta(String),
    ReasoningDelta {
        text: String,
        signature: Option<String>,
    },
    ToolCallStart {
        id: String,
        name: String,
    },
    ToolCallDelta {
        id: String,
        args_delta: String,
    },
    ToolCallEnd {
        id: String,
        name: String,
        arguments: String,
    },
    ToolResult {
        call_id: String,
        result: String,
    },
    /// 任务质量闭环（A 阶段）：工具执行后质量策略产出的 finding。
    /// 供前端渲染与后续阶段（B/C）消费。
    QualityFinding(QualityFinding),
    /// 协议增强：阶段迁移事件（供前端渲染 + 度量）。
    PhaseTransition {
        transition: PhaseTransition,
    },
    /// 协议增强：门控违规记录（供评分卡 protocol 维）。
    GateViolation(GateViolation),
    /// 协议增强：Execute 阶段 drift 检测产出（供前端渲染）。
    DriftFinding(DriftFinding),
    /// P4 完成前确定性验证：一条验证命令的结果（供前端渲染）。
    Verification {
        command: String,
        passed: bool,
        summary: String,
    },
    Usage(Usage),
    TurnComplete,
    ApprovalRequest {
        id: String,
        title: String,
        description: Option<String>,
    },
    /// The run stopped gracefully before completion (max-steps pause or
    /// budget rejection). The task is resumable: frontends should surface
    /// `reason` and, when present, which saved session to resume.
    Paused {
        reason: String,
        session_id: Option<String>,
    },
    Done(RunOutput),
}

// ---------------------------------------------------------------------------
// WireEvent — cross-frontend serializable event format
// Shared by Serve (SSE), CLI/TUI
// ---------------------------------------------------------------------------

/// A single event serialized for frontend consumption.
/// The `kind` field discriminates the event type.
/// This is the standard wire format that all frontends consume.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WireEvent {
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
        signature: Option<String>,
    },
    ToolCallStart {
        id: String,
        name: String,
    },
    ToolCallDelta {
        id: String,
        args_delta: String,
    },
    ToolCallEnd {
        id: String,
        name: String,
        arguments: String,
    },
    ToolResult {
        call_id: String,
        result: String,
    },
    QualityFinding {
        finding: QualityFinding,
    },
    PhaseTransition {
        transition: PhaseTransition,
    },
    GateViolation {
        violation: GateViolation,
    },
    DriftFinding {
        drift: DriftFinding,
    },
    Verification {
        command: String,
        passed: bool,
        summary: String,
    },
    Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
        cache_hit_tokens: u32,
        cache_miss_tokens: u32,
        /// DeepSeek-V4 billed reasoning (chain-of-thought) tokens.
        reasoning_tokens: u32,
        session_cache_hit_tokens: u32,
        session_cache_miss_tokens: u32,
    },
    TurnComplete,
    ApprovalRequest {
        id: String,
        title: String,
        description: Option<String>,
    },
    Done {
        text: String,
        usage: Option<WireUsageInfo>,
    },
    Paused {
        reason: String,
        session_id: Option<String>,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireUsageInfo {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub cache_hit_tokens: u32,
    pub cache_miss_tokens: u32,
    /// DeepSeek-V4 billed reasoning (chain-of-thought) tokens.
    pub reasoning_tokens: u32,
    pub session_cache_hit_tokens: u32,
    pub session_cache_miss_tokens: u32,
}

impl From<Usage> for WireUsageInfo {
    fn from(u: Usage) -> Self {
        Self {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
            cache_hit_tokens: u.cache_hit_tokens,
            cache_miss_tokens: u.cache_miss_tokens,
            reasoning_tokens: u.reasoning_tokens,
            session_cache_hit_tokens: 0,
            session_cache_miss_tokens: 0,
        }
    }
}

impl From<RunEvent> for WireEvent {
    fn from(event: RunEvent) -> Self {
        match event {
            RunEvent::TextDelta(text) => WireEvent::TextDelta { text },
            RunEvent::ReasoningDelta { text, signature } => {
                WireEvent::ReasoningDelta { text, signature }
            }
            RunEvent::ToolCallStart { id, name } => WireEvent::ToolCallStart { id, name },
            RunEvent::ToolCallDelta { id, args_delta } => {
                WireEvent::ToolCallDelta { id, args_delta }
            }
            RunEvent::ToolCallEnd {
                id,
                name,
                arguments,
            } => WireEvent::ToolCallEnd {
                id,
                name,
                arguments,
            },
            RunEvent::ToolResult { call_id, result } => WireEvent::ToolResult { call_id, result },
            RunEvent::QualityFinding(f) => WireEvent::QualityFinding { finding: f },
            RunEvent::PhaseTransition { transition } => WireEvent::PhaseTransition { transition },
            RunEvent::GateViolation(v) => WireEvent::GateViolation { violation: v },
            RunEvent::DriftFinding(d) => WireEvent::DriftFinding { drift: d },
            RunEvent::Verification {
                command,
                passed,
                summary,
            } => WireEvent::Verification {
                command,
                passed,
                summary,
            },
            RunEvent::Usage(u) => {
                let usage_info: WireUsageInfo = u.into();
                WireEvent::Usage {
                    prompt_tokens: usage_info.prompt_tokens,
                    completion_tokens: usage_info.completion_tokens,
                    total_tokens: usage_info.total_tokens,
                    cache_hit_tokens: usage_info.cache_hit_tokens,
                    cache_miss_tokens: usage_info.cache_miss_tokens,
                    reasoning_tokens: usage_info.reasoning_tokens,
                    session_cache_hit_tokens: usage_info.session_cache_hit_tokens,
                    session_cache_miss_tokens: usage_info.session_cache_miss_tokens,
                }
            }
            RunEvent::TurnComplete => WireEvent::TurnComplete,
            RunEvent::ApprovalRequest {
                id,
                title,
                description,
            } => WireEvent::ApprovalRequest {
                id,
                title,
                description,
            },
            RunEvent::Paused { reason, session_id } => WireEvent::Paused { reason, session_id },
            RunEvent::Done(output) => WireEvent::Done {
                text: output.text,
                usage: output.usage.map(|u| u.into()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paused_event_maps_to_wire() {
        let ev = RunEvent::Paused {
            reason: "reached max steps (10)".into(),
            session_id: Some("chat-20260729-120000".into()),
        };
        let wire: WireEvent = ev.into();
        match wire {
            WireEvent::Paused { reason, session_id } => {
                assert_eq!(reason, "reached max steps (10)");
                assert_eq!(session_id.as_deref(), Some("chat-20260729-120000"));
            }
            other => panic!("expected Paused, got {other:?}"),
        }
    }

    #[test]
    fn paused_wire_event_serializes_with_kind_tag() {
        let wire = WireEvent::Paused {
            reason: "budget: over limit".into(),
            session_id: None,
        };
        let json = serde_json::to_string(&wire).unwrap();
        assert!(json.contains("\"kind\":\"paused\""), "json = {json}");
    }

    #[test]
    fn quality_finding_event_maps_to_wire() {
        let f = crate::tool_hook::QualityFinding {
            rule: "no-commit-secret".into(),
            severity: crate::tool_hook::FindingSeverity::Blocking,
            passed: false,
            evidence: "-----BEGIN RSA PRIVATE KEY-----".into(),
        };
        let ev = RunEvent::QualityFinding(f.clone());
        let wire: WireEvent = ev.into();
        match wire {
            WireEvent::QualityFinding { finding } => assert_eq!(finding, f),
            other => panic!("expected QualityFinding, got {other:?}"),
        }
        let json = serde_json::to_string(&WireEvent::QualityFinding { finding: f }).unwrap();
        assert!(
            json.contains("\"kind\":\"quality_finding\""),
            "json = {json}"
        );
    }
}
