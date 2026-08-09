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
    ///
    /// 返回 [`crate::DeepseeknovaError`] 而非 `anyhow::Error`。stream 的
    /// item 类型也是 `Result<RunEvent, DeepseeknovaError>`——Phase 4
    /// keystone 变更后，stream 内部错误统一使用 `DeepseeknovaError`，
    /// 全链路不再依赖 `anyhow::Result`。
    async fn run_stream(&self, input: RunInput)
        -> Result<RunEventStream, crate::DeepseeknovaError>;

    /// Convenience: collect the stream into a final output.
    async fn run(&self, input: RunInput) -> Result<RunOutput, crate::DeepseeknovaError> {
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

/// Runner 一次执行的输入。
#[derive(Debug, Clone)]
pub struct RunInput {
    /// 用户输入的提示文本。
    pub prompt: String,
    /// 附加图像列表（data: URL 形式，供视觉模型消费）。
    pub images: Vec<String>, // data: URLs for vision-capable models
    /// 模型覆盖（运行时指定模型，覆盖默认配置）。
    pub model_override: Option<String>,
}

/// Runner 一次执行的最终输出。
#[derive(Debug, Clone)]
pub struct RunOutput {
    /// 模型产出的文本。
    pub text: String,
    /// 模型请求的工具调用列表。
    pub tool_calls: Vec<ToolCall>,
    /// 本次执行的 token 用量（若可用）。
    pub usage: Option<Usage>,
}

/// 运行事件流类型（boxed + pinned 的异步流）。
///
/// Phase 4 keystone：item 类型从 `anyhow::Result<RunEvent>` 迁移到
/// `Result<RunEvent, DeepseeknovaError>`，全链路统一错误类型。
pub type RunEventStream =
    Pin<Box<dyn Stream<Item = Result<RunEvent, crate::DeepseeknovaError>> + Send>>;

/// Resolves a permission-gate `Ask` decision by asking a frontend for a user
/// decision. Returning `true` allows the pending tool call; `false` denies it.
///
/// Frontends that can prompt a user (desktop app, HTTP server) implement this.
/// When no responder is attached, the agent falls back to allowing `Ask`
/// decisions so non-interactive callers (CLI, tests) keep working.
#[async_trait::async_trait]
pub trait ApprovalResponder: Send + Sync {
    /// 请求用户对一次待审批工具调用的裁决。
    ///
    /// 返回 `true` 表示允许执行，`false` 表示拒绝。
    async fn request(&self, id: &str, title: &str, description: Option<&str>) -> bool;
}

/// RunEvent has no Error variant — errors ride the Stream's Result.
#[derive(Debug, Clone)]
pub enum RunEvent {
    /// 增量文本片段。
    TextDelta(String),
    /// 增量推理（chain-of-thought）片段，附可选签名。
    ReasoningDelta {
        /// 推理文本片段。
        text: String,
        /// 推理签名（部分模型用于校验推理完整性）。
        signature: Option<String>,
    },
    /// 工具调用开始（携带 id 与工具名）。
    ToolCallStart {
        /// 工具调用 id。
        id: String,
        /// 工具名。
        name: String,
    },
    /// 工具调用参数增量片段。
    ToolCallDelta {
        /// 工具调用 id。
        id: String,
        /// 参数增量片段。
        args_delta: String,
    },
    /// 工具调用结束（携带完整参数）。
    ToolCallEnd {
        /// 工具调用 id。
        id: String,
        /// 工具名。
        name: String,
        /// 完整参数 JSON 字符串。
        arguments: String,
    },
    /// 工具执行结果（供前端回显）。
    ToolResult {
        /// 对应的工具调用 id。
        call_id: String,
        /// 工具结果文本。
        result: String,
    },
    /// 任务质量闭环（A 阶段）：工具执行后质量策略产出的 finding。
    /// 供前端渲染与后续阶段（B/C）消费。
    QualityFinding(QualityFinding),
    /// 协议增强：阶段迁移事件（供前端渲染 + 度量）。
    PhaseTransition {
        /// 阶段迁移详情。
        transition: PhaseTransition,
    },
    /// 协议增强：门控违规记录（供评分卡 protocol 维）。
    GateViolation(GateViolation),
    /// 协议增强：Execute 阶段 drift 检测产出（供前端渲染）。
    DriftFinding(DriftFinding),
    /// P4 完成前确定性验证：一条验证命令的结果（供前端渲染）。
    Verification {
        /// 验证命令。
        command: String,
        /// 是否通过。
        passed: bool,
        /// 结果摘要。
        summary: String,
    },
    /// 本次执行的 token 用量。
    Usage(Usage),
    /// 一个对话轮次完成。
    TurnComplete,
    /// 请求用户审批一次待执行的工具调用。
    ApprovalRequest {
        /// 审批请求 id。
        id: String,
        /// 审批标题。
        title: String,
        /// 审批描述（可选）。
        description: Option<String>,
    },
    /// The run stopped gracefully before completion (max-steps pause or
    /// budget rejection). The task is resumable: frontends should surface
    /// `reason` and, when present, which saved session to resume.
    Paused {
        /// 暂停原因。
        reason: String,
        /// 可恢复的会话 id（供前端 resume）。
        session_id: Option<String>,
    },
    /// 执行完成，携带最终输出。
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
    /// 增量文本片段。
    TextDelta {
        /// 文本片段。
        text: String,
    },
    /// 增量推理片段，附可选签名。
    ReasoningDelta {
        /// 推理文本片段。
        text: String,
        /// 推理签名（部分模型用于校验推理完整性）。
        signature: Option<String>,
    },
    /// 工具调用开始。
    ToolCallStart {
        /// 工具调用 id。
        id: String,
        /// 工具名。
        name: String,
    },
    /// 工具调用参数增量片段。
    ToolCallDelta {
        /// 工具调用 id。
        id: String,
        /// 参数增量片段。
        args_delta: String,
    },
    /// 工具调用结束（携带完整参数）。
    ToolCallEnd {
        /// 工具调用 id。
        id: String,
        /// 工具名。
        name: String,
        /// 完整参数 JSON 字符串。
        arguments: String,
    },
    /// 工具执行结果。
    ToolResult {
        /// 对应的工具调用 id。
        call_id: String,
        /// 工具结果文本。
        result: String,
    },
    /// 任务质量闭环 finding。
    QualityFinding {
        /// 质量策略评估结果。
        finding: QualityFinding,
    },
    /// 协议阶段迁移事件。
    PhaseTransition {
        /// 阶段迁移详情。
        transition: PhaseTransition,
    },
    /// 协议门控违规记录。
    GateViolation {
        /// 门控违规详情。
        violation: GateViolation,
    },
    /// Execute 阶段 drift 检测产出。
    DriftFinding {
        /// drift 检测详情。
        drift: DriftFinding,
    },
    /// P4 完成前确定性验证结果。
    Verification {
        /// 验证命令。
        command: String,
        /// 是否通过。
        passed: bool,
        /// 结果摘要。
        summary: String,
    },
    /// 本次执行的 token 用量。
    Usage {
        /// 提示 token 数。
        prompt_tokens: u32,
        /// 生成 token 数。
        completion_tokens: u32,
        /// 总 token 数。
        total_tokens: u32,
        /// 单请求级缓存命中 token 数。
        cache_hit_tokens: u32,
        /// 单请求级缓存未命中 token 数。
        cache_miss_tokens: u32,
        /// DeepSeek-V4 billed reasoning (chain-of-thought) tokens.
        reasoning_tokens: u32,
        /// 会话级（跨轮次）缓存命中 token 数。当前恒为 0，
        /// 真实统计见 \[规划中\]（与 [`WireUsageInfo::session_cache_hit_tokens`]
        /// 的说明一致）。
        session_cache_hit_tokens: u32,
        /// 会话级（跨轮次）缓存未命中 token 数。当前恒为 0
        /// （见 [`WireUsageInfo::session_cache_hit_tokens`] 的说明）。
        session_cache_miss_tokens: u32,
    },
    /// 一个对话轮次完成。
    TurnComplete,
    /// 请求用户审批一次待执行的工具调用。
    ApprovalRequest {
        /// 审批请求 id。
        id: String,
        /// 审批标题。
        title: String,
        /// 审批描述（可选）。
        description: Option<String>,
    },
    /// 执行完成，携带最终输出与可选用量。
    Done {
        /// 最终文本输出。
        text: String,
        /// token 用量（若可用）。
        usage: Option<WireUsageInfo>,
        /// 关联 run/会话/metrics 的关联键：serve 透传当前 run 对应的
        /// session_id（`/v1/sessions/{id}/chat` 为会话 id，`/v1/chat` 与
        /// `/v1/runs/{id}/resume` 为 durable run id），供前端据此拉取
        /// 该 run 的评分卡/诊断。`From<RunEvent>` 转换在无传输上下文时
        /// 置 `None`；由传输层（serve SSE）注入。serde default 保证旧格式
        /// 事件（无该字段）仍可反序列化。
        #[serde(default)]
        session_id: Option<String>,
    },
    /// 执行暂停（可恢复）。
    Paused {
        /// 暂停原因。
        reason: String,
        /// 可恢复的会话 id。
        session_id: Option<String>,
    },
    /// 执行出错。
    Error {
        /// 错误信息。
        message: String,
    },
}

/// wire 格式的 token 用量信息（`WireEvent::Usage` 与 `WireEvent::Done` 携带）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireUsageInfo {
    /// 提示 token 数。
    pub prompt_tokens: u32,
    /// 生成 token 数。
    pub completion_tokens: u32,
    /// 总 token 数。
    pub total_tokens: u32,
    /// 单请求级缓存命中 token 数（来自 provider API 的 usage）。
    pub cache_hit_tokens: u32,
    /// 单请求级缓存未命中 token 数（来自 provider API 的 usage）。
    pub cache_miss_tokens: u32,
    /// DeepSeek-V4 billed reasoning (chain-of-thought) tokens.
    pub reasoning_tokens: u32,
    /// 会话级（跨轮次）缓存命中 token 数。
    ///
    /// **当前恒为 0**：真实会话级命中率统计 \[规划中\]，尚未落地；
    /// 该字段仅为向前兼容保留（serde 契约，不破坏消费方/旧数据）。
    /// 单请求级缓存命中请使用 [`WireUsageInfo::cache_hit_tokens`]。
    pub session_cache_hit_tokens: u32,
    /// 会话级（跨轮次）缓存未命中 token 数。当前恒为 0
    /// （见 [`WireUsageInfo::session_cache_hit_tokens`] 的说明）。
    pub session_cache_miss_tokens: u32,
}

impl From<Usage> for WireUsageInfo {
    /// 将 provider usage 转为 wire 用量信息（会话级缓存字段当前置 0）。
    fn from(u: Usage) -> Self {
        Self {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
            cache_hit_tokens: u.cache_hit_tokens,
            cache_miss_tokens: u.cache_miss_tokens,
            reasoning_tokens: u.reasoning_tokens,
            // 会话级命中率统计 [规划中]：当前恒为 0，
            // 见 WireUsageInfo::session_cache_hit_tokens 的 doc 说明。
            session_cache_hit_tokens: 0,
            session_cache_miss_tokens: 0,
        }
    }
}

impl From<RunEvent> for WireEvent {
    /// 将运行时事件转为前端消费的 wire 格式。
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
                // RunEvent::Done 不携带传输上下文 id；由 serve 传输层在
                // SSE done 事件中注入 session_id（见 serve::map_run_event）。
                session_id: None,
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

    /// B2：done 事件携带 `session_id` 关联键。`From<RunEvent>` 转换在无传输
    /// 上下文时置 `None`（RunEvent::Done 不含 id，由 serve 传输层注入）；
    /// 显式构造带 session_id 的 `WireEvent::Done` 须在 wire JSON 中序列化该
    /// 字段，供前端据此拉取该 run 的评分卡/诊断。
    #[test]
    fn done_wire_event_carries_session_id() {
        // 传输层注入路径：显式构造的 done 事件必须带 session_id。
        let wire = WireEvent::Done {
            text: "done".into(),
            usage: None,
            session_id: Some("session-1722830400-0".into()),
        };
        let json = serde_json::to_string(&wire).unwrap();
        assert!(
            json.contains("\"kind\":\"done\"")
                && json.contains("\"session_id\":\"session-1722830400-0\""),
            "done JSON must carry session_id, json = {json}"
        );
        // 往返：反序列化不丢字段。
        let back: WireEvent = serde_json::from_str(&json).unwrap();
        match back {
            WireEvent::Done { session_id, .. } => {
                assert_eq!(session_id.as_deref(), Some("session-1722830400-0"))
            }
            other => panic!("expected Done, got {other:?}"),
        }

        // From<RunEvent> 无传输上下文 → session_id 为 None（serve 层负责注入）。
        let output = RunOutput {
            text: "done".into(),
            tool_calls: Vec::new(),
            usage: None,
        };
        let wire: WireEvent = RunEvent::Done(output).into();
        match wire {
            WireEvent::Done { session_id, .. } => assert_eq!(session_id, None),
            other => panic!("expected Done, got {other:?}"),
        }

        // 旧格式 JSON（无 session_id 字段）仍可反序列化（serde default）。
        let legacy = r#"{"kind":"done","text":"hi","usage":null}"#;
        let back: WireEvent = serde_json::from_str(legacy).unwrap();
        match back {
            WireEvent::Done { session_id, .. } => assert_eq!(session_id, None),
            other => panic!("expected Done, got {other:?}"),
        }
    }
}
