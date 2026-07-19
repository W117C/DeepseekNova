use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// DeepSeek reasoning content — must be passed back to the API
    /// in subsequent turns when tool calls are involved (otherwise 400 error).
    /// When no tool calls were made, this field is ignored by the API.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
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
    pub text: String,
    /// True when this reasoning is paired with tool calls in the same turn.
    /// History compression must respect this — never drop reasoning alone.
    pub must_replay: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub ty: String, // typically "function"
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// Schema definition for a tool exposed to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Cache-shape diagnostics
// ---------------------------------------------------------------------------

/// Captures the shape of the prefix sent to the LLM on each turn.
/// Comparing consecutive shapes explains why cache misses occur.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrefixShape {
    /// Hash of the combined system prompt content.
    pub system_hash: String,
    /// Hash of the sorted tool schema names (tool ordering stability).
    pub tools_hash: String,
    /// Number of tool schemas included.
    pub tool_count: usize,
    /// Number of messages in the request.
    pub message_count: usize,
    /// Estimated token count sent.
    pub estimated_tokens: u32,
}

impl PrefixShape {
    /// Compute a shape from the messages and tool schemas.
    pub fn capture(messages: &[Message], tools: &[ToolSchema]) -> Self {
        use sha2::{Digest, Sha256};

        let system_hash = {
            let sys: String = messages
                .iter()
                .filter(|m| m.role == Role::System)
                .map(|m| &m.content)
                .cloned()
                .collect::<Vec<_>>()
                .join("");
            hex::encode(Sha256::digest(sys.as_bytes()))
        };

        let mut tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        tool_names.sort();
        let tools_hash = hex::encode(Sha256::digest(tool_names.join(",").as_bytes()));

        let char_count: usize = messages.iter().map(|m| m.content.len()).sum();
        let estimated_tokens = (char_count as f32 / 4.0).ceil() as u32;

        Self {
            system_hash,
            tools_hash,
            tool_count: tools.len(),
            message_count: messages.len(),
            estimated_tokens,
        }
    }

    /// Human-readable diff between two shapes.
    pub fn diff(&self, other: &PrefixShape) -> String {
        let mut changes = Vec::new();
        if self.system_hash != other.system_hash {
            changes.push("system prompt changed".to_string());
        }
        if self.tools_hash != other.tools_hash {
            changes.push(format!(
                "tools changed: {} → {} tools",
                self.tool_count, other.tool_count
            ));
        }
        if self.message_count != other.message_count {
            changes.push(format!(
                "messages: {} → {}",
                self.message_count, other.message_count
            ));
        }
        if self.estimated_tokens != other.estimated_tokens {
            changes.push(format!(
                "tokens: {} → {} ({:+.0}%)",
                self.estimated_tokens,
                other.estimated_tokens,
                if self.estimated_tokens > 0 {
                    (other.estimated_tokens as f64 / self.estimated_tokens as f64 - 1.0) * 100.0
                } else {
                    0.0
                }
            ));
        }
        if changes.is_empty() {
            "no change".to_string()
        } else {
            changes.join("; ")
        }
    }
}
