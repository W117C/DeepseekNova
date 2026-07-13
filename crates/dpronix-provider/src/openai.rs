use crate::types::{ChatCompletionResponse, OpenAIFunction, OpenAIRequestTool, StreamResponse};
use crate::{Provider, ProviderError};
use anyhow::Context;
use async_trait::async_trait;
use dpronix_core::chunk::{Chunk, ChunkStream, Usage};
use dpronix_core::{Message, Tool};
use reqwest::Client;
use std::env;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tracing::info;

pub struct OpenAIProvider {
    client: Client,
    base_url: String,
    model: String,
    api_key: String,
    /// Reasoning effort for DeepSeek models: "low" | "medium" | "high"
    reasoning_effort: Option<String>,
    /// Enable DeepSeek thinking mode (extra_body: {"thinking": {"type": "enabled"}})
    thinking_enabled: bool,
    /// Extra JSON body fields to include in every request
    extra_body: Option<serde_json::Value>,
}

impl OpenAIProvider {
    pub fn new(
        base_url: &str,
        model: &str,
        api_key_env: &str,
        timeout_secs: u64,
        _max_retries: u32,
    ) -> anyhow::Result<Self> {
        let api_key = env::var(api_key_env)
            .with_context(|| format!("environment variable {api_key_env} is not set"))?;

        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            api_key,
            reasoning_effort: None,
            thinking_enabled: false,
            extra_body: None,
        })
    }

    /// Enable DeepSeek reasoning mode with the given effort level.
    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Self {
        self.reasoning_effort = Some(effort.into());
        self
    }

    /// Enable DeepSeek thinking mode.
    pub fn with_thinking(mut self, enabled: bool) -> Self {
        self.thinking_enabled = enabled;
        self
    }

    /// Set extra body fields to include in every request.
    pub fn with_extra_body(mut self, body: Option<serde_json::Value>) -> Self {
        self.extra_body = body;
        self
    }

    fn build_tools(&self, tools: &[&dyn Tool]) -> Option<Vec<OpenAIRequestTool>> {
        let schemas: Vec<_> = tools.iter().map(|t| t.schema()).collect();
        if schemas.is_empty() {
            return None;
        }
        let oai_tools: Vec<OpenAIRequestTool> = schemas
            .iter()
            .map(|s| OpenAIRequestTool {
                ty: "function".to_string(),
                function: OpenAIFunction {
                    name: s.name.clone(),
                    description: s.description.clone(),
                    parameters: s.parameters.clone(),
                },
            })
            .collect();
        Some(oai_tools)
    }

    fn build_request(
        &self,
        messages: &[Message],
        tools: &[&dyn Tool],
        stream: bool,
    ) -> serde_json::Value {
        // Merge extra_body with thinking mode parameter
        let mut extra = self
            .extra_body
            .clone()
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
        if self.thinking_enabled {
            if let serde_json::Value::Object(ref mut map) = extra {
                map.insert(
                    "thinking".to_string(),
                    serde_json::json!({"type": "enabled"}),
                );
            }
        }
        let extra_body = if extra.as_object().is_some_and(|o| !o.is_empty()) {
            Some(extra)
        } else {
            None
        };

        let mut req = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "stream": stream,
        });
        if let Some(tools) = self.build_tools(tools) {
            req["tools"] = serde_json::json!(tools);
        }
        if let Some(ref effort) = self.reasoning_effort {
            req["reasoning_effort"] = serde_json::json!(effort);
        }
        if let Some(serde_json::Value::Object(ref eb_map)) = extra_body {
            for (k, v) in eb_map {
                req[k] = v.clone();
            }
        }
        req
    }

    async fn send_request(&self, body: &serde_json::Value) -> anyhow::Result<reqwest::Response> {
        let url = format!("{}/chat/completions", self.base_url);

        info!(
            "POST {} (stream={})",
            url,
            body.get("stream")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        );

        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(body)
            .send()
            .await
            .context("failed to send request to provider")?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Http {
                status: status.as_u16(),
                body: error_text,
            }
            .into());
        }

        Ok(response)
    }
}

#[async_trait]
impl Provider for OpenAIProvider {
    async fn generate(&self, messages: &[Message], tools: &[&dyn Tool]) -> anyhow::Result<Message> {
        let body = self.build_request(messages, tools, false);
        let response = self.send_request(&body).await?;

        let resp_body: ChatCompletionResponse = response
            .json()
            .await
            .context("failed to parse provider response")?;

        let choice = resp_body
            .choices
            .into_iter()
            .next()
            .ok_or(ProviderError::NoChoices)?;

        Ok(choice.message)
    }

    async fn stream(
        &self,
        messages: &[Message],
        tools: &[&dyn Tool],
    ) -> anyhow::Result<ChunkStream> {
        let body = self.build_request(messages, tools, true);
        let response = self.send_request(&body).await?;

        let (tx, rx) = mpsc::channel(64);

        // Spawn a task that reads the SSE stream chunk-by-chunk and feeds
        // parsed Chunks into the channel. This gives us true streaming
        // instead of buffering the entire response body.
        tokio::spawn(async move {
            if let Err(e) = stream_sse_response(response, &tx).await {
                let _ = tx.send(Err(e)).await;
            }
        });

        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }
}

// ---------------------------------------------------------------------------
// SSE streaming — reads the HTTP response body as a byte stream and
// emits parsed Chunks as they arrive.
// ---------------------------------------------------------------------------

/// Accumulator for a single streaming tool call.
#[derive(Debug, Default)]
struct AccToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
    /// Whether we've already emitted ToolCallStart for this index
    started: bool,
}

async fn stream_sse_response(
    response: reqwest::Response,
    tx: &mpsc::Sender<anyhow::Result<Chunk>>,
) -> anyhow::Result<()> {
    // Accumulate raw bytes per line to avoid UTF-8 corruption across TCP chunks
    let mut line_bytes: Vec<u8> = Vec::new();
    let mut tool_acc: Vec<AccToolCall> = Vec::new();

    let mut byte_stream = response.bytes_stream();

    while let Some(chunk_result) = byte_stream.next().await {
        let bytes = chunk_result.context("failed to read chunk from stream")?;

        for &b in bytes.iter() {
            match b {
                b'\n' => {
                    if line_bytes.is_empty() {
                        continue;
                    }
                    let line_str = String::from_utf8(line_bytes.clone())
                        .map_err(|e| anyhow::anyhow!("invalid UTF-8 in SSE stream: {e}"))?;
                    line_bytes.clear();
                    let trimmed = line_str.trim().to_string();
                    if trimmed.is_empty() {
                        continue;
                    }
                    process_sse_line(&trimmed, tx, &mut tool_acc).await?;
                }
                b'\r' => { /* skip — handled by \n */ }
                _ => line_bytes.push(b),
            }
        }
    }

    // Process any remaining buffered data
    if !line_bytes.is_empty() {
        let tail_str = String::from_utf8(line_bytes)
            .map_err(|e| anyhow::anyhow!("invalid UTF-8 in SSE stream tail: {e}"))?;
        let trimmed = tail_str.trim().to_string();
        if !trimmed.is_empty() {
            process_sse_line(&trimmed, tx, &mut tool_acc).await?;
        }
    }

    // Flush any pending tool calls
    flush_pending_tool_calls(tx, &mut tool_acc).await?;

    Ok(())
}

/// Process a single SSE line (without the trailing \n).
async fn process_sse_line(
    line: &str,
    tx: &mpsc::Sender<anyhow::Result<Chunk>>,
    tool_acc: &mut Vec<AccToolCall>,
) -> anyhow::Result<()> {
    // End-of-stream marker
    if line == "data: [DONE]" {
        flush_pending_tool_calls(tx, tool_acc).await?;
        let _ = tx.send(Ok(Chunk::Done)).await;
        return Ok(());
    }

    // Only process "data: ..." lines
    let Some(data) = line.strip_prefix("data: ") else {
        return Ok(()); // skip comments, keepalive (": keepalive"), etc.
    };

    // Try to parse the SSE JSON
    let Ok(resp) = serde_json::from_str::<StreamResponse>(data) else {
        return Ok(()); // skip unparseable lines (e.g. keepalive)
    };

    // Final usage chunk
    if let Some(ref u) = resp.usage {
        let _ = tx
            .send(Ok(Chunk::Usage(Usage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
                cache_hit_tokens: 0,
                cache_miss_tokens: 0,
                reasoning_tokens: 0,
            })))
            .await;
    }

    for choice in resp.choices {
        // Check for finish_reason = "tool_calls" to flush accumulated calls
        let is_tool_call_finish = choice.finish_reason.as_deref() == Some("tool_calls");

        if let Some(ref delta) = choice.delta {
            // --- Text content ---
            if let Some(ref content) = delta.content {
                if !content.is_empty() {
                    let _ = tx.send(Ok(Chunk::TextDelta(content.clone()))).await;
                }
            }

            // --- Reasoning content (DeepSeek thinking mode) ---
            if let Some(ref reasoning) = delta.reasoning_content {
                if !reasoning.is_empty() {
                    let _ = tx
                        .send(Ok(Chunk::ReasoningDelta {
                            text: reasoning.clone(),
                            signature: None,
                        }))
                        .await;
                }
            }

            // --- Streaming tool calls ---
            if let Some(ref tool_calls) = delta.tool_calls {
                for tc in tool_calls {
                    let idx = tc.index as usize;
                    // Ensure accumulator has slots
                    while tool_acc.len() <= idx {
                        tool_acc.push(AccToolCall::default());
                    }
                    let acc = &mut tool_acc[idx];

                    // First delta for this tool call: emit ToolCallStart
                    if let Some(ref id) = tc.id {
                        if !acc.started {
                            acc.started = true;
                            acc.id = Some(id.clone());
                            if let Some(ref func) = tc.function {
                                if let Some(ref name) = func.name {
                                    acc.name = Some(name.clone());
                                    let _ = tx
                                        .send(Ok(Chunk::ToolCallStart {
                                            id: id.clone(),
                                            name: name.clone(),
                                        }))
                                        .await;
                                }
                            }
                        }
                    }

                    // Accumulate argument deltas
                    if let Some(ref func) = tc.function {
                        if let Some(ref args) = func.arguments {
                            if !args.is_empty() {
                                let call_id = acc.id.clone().unwrap_or_default();
                                let _ = tx
                                    .send(Ok(Chunk::ToolCallDelta {
                                        id: call_id.clone(),
                                        args_delta: args.clone(),
                                    }))
                                    .await;
                                acc.arguments.push_str(args);
                            }
                        }
                    }
                }
            }
        }

        // On finish_reason = "tool_calls", emit accumulated ToolCallEnd events
        if is_tool_call_finish {
            flush_pending_tool_calls(tx, tool_acc).await?;
        }
    }

    Ok(())
}

/// Emit ToolCallEnd for any pending (accumulated but not flushed) tool calls.
async fn flush_pending_tool_calls(
    tx: &mpsc::Sender<anyhow::Result<Chunk>>,
    tool_acc: &mut Vec<AccToolCall>,
) -> anyhow::Result<()> {
    for acc in tool_acc.drain(..) {
        if let (Some(id), Some(name)) = (acc.id, acc.name) {
            let _ = tx
                .send(Ok(Chunk::ToolCallEnd {
                    id,
                    name,
                    arguments: acc.arguments,
                }))
                .await;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use dpronix_core::tool::ToolContext;
    use dpronix_core::types::ToolSchema;

    #[allow(dead_code)]
    struct NoopTool;

    #[async_trait]
    impl Tool for NoopTool {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: "noop".into(),
                description: "does nothing".into(),
                parameters: serde_json::json!({"type":"object","properties":{}}),
            }
        }

        async fn execute(&self, _ctx: &ToolContext, _args: &str) -> anyhow::Result<String> {
            Ok("ok".into())
        }
    }

    /// Verify that SSE text without tool calls is parsed into Chunks.
    #[tokio::test]
    async fn parse_sse_text_content() {
        let sse_data = r#"data: {"choices":[{"index":0,"delta":{"content":"Hello"}}]}

data: {"choices":[{"index":0,"delta":{"content":" world"}}]}

data: [DONE]
"#;

        let (tx, mut rx) = mpsc::channel(64);
        let mut tool_acc = Vec::new();

        for line in sse_data.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            process_sse_line(trimmed, &tx, &mut tool_acc).await.unwrap();
        }

        drop(tx);

        let mut chunks = Vec::new();
        while let Some(chunk) = rx.recv().await {
            chunks.push(chunk.unwrap());
        }

        // Should have: TextDelta("Hello"), TextDelta(" world"), Done
        let text_chunks: Vec<&str> = chunks
            .iter()
            .filter_map(|c| {
                if let Chunk::TextDelta(t) = c {
                    Some(t.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            text_chunks,
            vec!["Hello", " world"],
            "should parse two text deltas"
        );
        assert!(
            chunks.iter().any(|c| matches!(c, Chunk::Done)),
            "should end with Done"
        );
    }

    /// Verify that streaming tool_calls are accumulated into ToolCallEnd.
    #[tokio::test]
    async fn parse_sse_tool_calls() {
        let sse_data = r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read_file","arguments":""}}]}}]}

data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":\"src"}}]}}]}

data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"/main.rs\"}"}}]}}]}

data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}

data: [DONE]
"#;

        let (tx, mut rx) = mpsc::channel(64);
        let mut tool_acc = Vec::new();

        for line in sse_data.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            process_sse_line(trimmed, &tx, &mut tool_acc).await.unwrap();
        }

        drop(tx);

        let mut chunks = Vec::new();
        while let Some(chunk) = rx.recv().await {
            chunks.push(chunk.unwrap());
        }

        // Should have: ToolCallStart, ToolCallDelta, ToolCallDelta, ToolCallEnd, Done
        let has_start = chunks
            .iter()
            .any(|c| matches!(c, Chunk::ToolCallStart { name, .. } if name == "read_file"));
        let has_end = chunks
            .iter()
            .any(|c| matches!(c, Chunk::ToolCallEnd { name, .. } if name == "read_file"));
        assert!(has_start, "should emit ToolCallStart for read_file");
        assert!(has_end, "should emit ToolCallEnd for read_file");

        // Find the ToolCallEnd and verify accumulated arguments
        for chunk in &chunks {
            if let Chunk::ToolCallEnd { arguments, .. } = chunk {
                assert!(
                    arguments.contains("src/main.rs"),
                    "arguments should be fully accumulated"
                );
                break;
            }
        }
    }

    /// Verify reasoning_content is parsed.
    #[tokio::test]
    async fn parse_sse_reasoning_content() {
        let sse_data = r#"data: {"choices":[{"index":0,"delta":{"reasoning_content":"thinking step 1..."}}]}

data: {"choices":[{"index":0,"delta":{"content":"Final answer"}}]}

data: [DONE]
"#;

        let (tx, mut rx) = mpsc::channel(64);
        let mut tool_acc = Vec::new();

        for line in sse_data.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            process_sse_line(trimmed, &tx, &mut tool_acc).await.unwrap();
        }

        drop(tx);

        let mut chunks = Vec::new();
        while let Some(chunk) = rx.recv().await {
            chunks.push(chunk.unwrap());
        }

        // Should have: ReasoningDelta, TextDelta, Done
        let has_reasoning = chunks.iter().any(
            |c| matches!(c, Chunk::ReasoningDelta { text, .. } if text == "thinking step 1..."),
        );
        let has_text = chunks
            .iter()
            .any(|c| matches!(c, Chunk::TextDelta(t) if t == "Final answer"));
        assert!(
            has_reasoning,
            "should parse reasoning_content as ReasoningDelta"
        );
        assert!(has_text, "should parse content as TextDelta");
    }
}
