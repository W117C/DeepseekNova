use serde::{Deserialize, Serialize};

/// 对话消息的角色类型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// 系统指令角色。
    System,
    /// 用户角色。
    User,
    /// 助手角色。
    Assistant,
    /// 工具调用结果角色。
    Tool,
}

/// 一条对话消息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// 消息角色。
    pub role: Role,
    /// 消息文本内容。
    pub content: String,
    /// 可选的消息作者名称。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 助手发起的工具调用列表。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// 该工具结果对应的工具调用 ID（仅 Tool 角色消息使用）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// DeepSeek reasoning content — must be passed back to the API
    /// in subsequent turns when tool calls are involved (otherwise 400 error).
    /// When no tool calls were made, this field is ignored by the API.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    /// Anthropic/DeepSeek 兼容端点的 thinking 块签名（opaque signature）。
    ///
    /// 多轮对话中 assistant 的 thinking 块必须**原样回传**（含 signature），
    /// 否则 `api.anthropic.com` 与 `api.deepseek.com/anthropic` 均以 HTTP 400
    /// 拒绝（"The content[].thinking in the thinking mode must be passed back
    /// to the API."）。签名由响应解析填充（非流式 thinking 块的 `signature`
    /// 字段 / 流式 `signature_delta` 事件）；无签名（如 OpenAI 端点）为
    /// `None`，回放时不带 signature 字段。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_signature: Option<String>,
}

impl Message {
    /// Extract the reasoning block with its must_replay constraint.
    ///
    /// `must_replay` is true when the assistant message produced tool calls
    /// AND has reasoning content — DeepSeek V4 requires this reasoning to be
    /// present in all subsequent requests. History compaction must never
    /// drop reasoning without also dropping the paired tool_calls + results.
    pub fn reasoning_block(&self) -> Option<ReasoningBlock> {
        self.reasoning_content.as_ref().map(|text| ReasoningBlock {
            text: text.clone(),
            must_replay: self
                .tool_calls
                .as_ref()
                .map(|tc| !tc.is_empty())
                .unwrap_or(false),
        })
    }
}

/// A reasoning block extracted from an assistant message.
///
/// Carries the `must_replay` constraint: when true, history compaction
/// must either preserve the entire (reasoning + tool_calls + tool_results)
/// triple together, or remove it atomically. Partial removal of reasoning
/// while keeping tool calls causes DeepSeek V4 to return HTTP 400.
#[derive(Debug, Clone)]
pub struct ReasoningBlock {
    /// 推理文本内容。
    pub text: String,
    /// True when this reasoning is paired with tool calls in the same turn.
    /// History compression must respect this — never drop reasoning alone.
    pub must_replay: bool,
}

/// 一次工具调用请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// 工具调用 ID（用于关联后续的 Tool 结果）。
    pub id: String,
    /// 调用类型，通常为 "function"。
    #[serde(rename = "type")]
    pub ty: String, // typically "function"
    /// 被调用的函数及参数。
    pub function: FunctionCall,
}

/// 函数调用的名称与参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    /// 函数名。
    pub name: String,
    /// 函数参数（JSON 字符串）。
    pub arguments: String,
}

/// Schema definition for a tool exposed to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    /// 工具名称。
    pub name: String,
    /// 工具描述。
    pub description: String,
    /// 参数 JSON Schema。
    pub parameters: serde_json::Value,
}
