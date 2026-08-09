use crate::retry::{retry_with_backoff, HttpAttempt};
use crate::tool_cache::ToolSchemaCache;
use crate::types::{ChatCompletionResponse, OpenAIFunction, OpenAIRequestTool, StreamResponse};
use crate::{Provider, ProviderError, ValidatedRequest};
use async_trait::async_trait;
use deepseeknova_core::chunk::{Chunk, ChunkStream};
use deepseeknova_core::{DeepseeknovaError, Message, Tool};
use reqwest::Client;
use std::env;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tracing::{info, warn};

pub struct OpenAIProvider {
    client: Client,
    base_url: String,
    model: String,
    api_key: String,
    /// Max retries for transient HTTP failures.
    max_retries: u32,
    /// Reasoning effort for DeepSeek models: "low" | "medium" | "high"
    reasoning_effort: Option<String>,
    /// Enable DeepSeek thinking mode (extra_body: {"thinking": {"type": "enabled"}})
    thinking_enabled: bool,
    /// Extra JSON body fields to include in every request
    extra_body: Option<serde_json::Value>,
    /// Cache of serialised tool-schema arrays, keyed by tool identity, so the
    /// per-request collect + sort + serialise is skipped when the tool set is
    /// unchanged.
    tool_cache: ToolSchemaCache<serde_json::Value>,
    /// Sampling temperature written into the request body when set.
    temperature: Option<f32>,
}

impl OpenAIProvider {
    pub fn new(
        base_url: &str,
        model: &str,
        api_key_env: &str,
        timeout_secs: u64,
        max_retries: u32,
    ) -> Result<Self, DeepseeknovaError> {
        let api_key = env::var(api_key_env).map_err(|_| {
            DeepseeknovaError::Config(format!("environment variable {api_key_env} is not set"))
        })?;

        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .map_err(|e| {
                DeepseeknovaError::provider(format!("failed to build HTTP client: {e}"))
            })?;

        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            api_key,
            max_retries,
            reasoning_effort: None,
            thinking_enabled: false,
            extra_body: None,
            tool_cache: ToolSchemaCache::with_capacity(16),
            temperature: None,
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

    /// Set the sampling temperature written into every request body.
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Build the OpenAI `tools` array for `tools`, cached by tool identity so
    /// an unchanged registry skips the collect + sort + serialise on the hot
    /// path. Returns `None` for an empty tool set (the field is then omitted).
    fn build_tools(&self, tools: &[&dyn Tool]) -> Option<serde_json::Value> {
        self.tool_cache.get_or_build(tools, |ts| {
            let mut schemas: Vec<_> = ts.iter().map(|t| t.schema()).collect();
            // Sort by name for cache-stable tool ordering
            schemas.sort_by(|a, b| a.name.cmp(&b.name));
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
            serde_json::json!(oai_tools)
        })
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
        // Ask DeepSeek to include usage statistics (cache hit/miss tokens +
        // reasoning tokens) in the final streaming chunk.
        if stream {
            req["stream_options"] = serde_json::json!({"include_usage": true});
        }
        if let Some(tools) = self.build_tools(tools) {
            req["tools"] = tools;
        }
        if let Some(ref effort) = self.reasoning_effort {
            req["reasoning_effort"] = serde_json::json!(effort);
        }
        if let Some(temp) = self.temperature {
            req["temperature"] = serde_json::json!(temp);
        }
        if let Some(serde_json::Value::Object(ref eb_map)) = extra_body {
            for (k, v) in eb_map {
                req[k] = v.clone();
            }
        }
        req
    }

    async fn send_request(
        &self,
        body: &serde_json::Value,
    ) -> Result<reqwest::Response, DeepseeknovaError> {
        send_chat_request(
            &self.client,
            &self.api_key,
            &self.base_url,
            self.max_retries,
            body,
        )
        .await
    }
}

/// POST a chat-completions payload with header-phase retry (429/5xx/network).
/// Retries cover everything up to response headers; body-read failures of a
/// streaming response are handled separately in `stream()` because retrying
/// after emitting chunks would duplicate output.
async fn send_chat_request(
    client: &Client,
    api_key: &str,
    base_url: &str,
    max_retries: u32,
    body: &serde_json::Value,
) -> Result<reqwest::Response, DeepseeknovaError> {
    let url = format!("{base_url}/chat/completions");

    let stream = body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    info!("POST {} (stream={})", url, stream);

    let result = retry_with_backoff(max_retries, || {
        let client = client.clone();
        let api_key = api_key.to_string();
        let body = body.clone();
        let url = url.clone();
        Box::pin(async move {
            match client
                .post(&url)
                .bearer_auth(&api_key)
                .json(&body)
                .send()
                .await
            {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        HttpAttempt::Success(response)
                    } else if crate::retry::is_retryable_status(status.as_u16()) {
                        let error_text = response.text().await.unwrap_or_default();
                        HttpAttempt::Retryable(format!("HTTP {status}: {error_text}"))
                    } else {
                        let error_text = response.text().await.unwrap_or_default();
                        HttpAttempt::Fatal(format!("HTTP {status}: {error_text}"))
                    }
                }
                Err(e) => {
                    let err_str = e.to_string();
                    if crate::retry::is_retryable_error(&err_str) {
                        HttpAttempt::Retryable(err_str)
                    } else {
                        HttpAttempt::Fatal(err_str)
                    }
                }
            }
        })
    })
    .await;

    match result {
        HttpAttempt::Success(response) => Ok(response),
        HttpAttempt::Retryable(msg) => Err(DeepseeknovaError::provider_retryable(format!(
            "request failed after retries: {msg}"
        ))),
        HttpAttempt::Fatal(msg) => Err(DeepseeknovaError::provider(msg)),
    }
}

#[async_trait]
impl Provider for OpenAIProvider {
    async fn generate(
        &self,
        validated: ValidatedRequest<'_>,
    ) -> Result<Message, DeepseeknovaError> {
        let messages = validated.messages;
        let tools = validated.tools;
        let body = self.build_request(messages, tools, false);
        let response = self.send_request(&body).await?;

        let resp_body: ChatCompletionResponse = response.json().await.map_err(|e| {
            DeepseeknovaError::provider(format!("failed to parse provider response: {e}"))
        })?;

        // Surface DeepSeek token accounting for auxiliary (non-streaming) calls
        // so context-cache efficiency and billed reasoning tokens stay visible
        // even though this path returns only the message body.
        if let Some(ref u) = resp_body.usage {
            info!(
                prompt_tokens = u.prompt_tokens,
                completion_tokens = u.completion_tokens,
                total_tokens = u.total_tokens,
                cache_hit_tokens = u.cache_hit_tokens,
                cache_miss_tokens = u.cache_miss_tokens,
                reasoning_tokens = u.reasoning_tokens(),
                "deepseek usage (non-streaming generate)"
            );
        }

        let choice = resp_body
            .choices
            .into_iter()
            .next()
            .ok_or(ProviderError::NoChoices)?;

        Ok(choice.message)
    }

    async fn stream(
        &self,
        validated: ValidatedRequest<'_>,
    ) -> Result<ChunkStream, DeepseeknovaError> {
        let messages = validated.messages;
        let tools = validated.tools;
        let body = self.build_request(messages, tools, true);

        // Body-read retry, separate from the header-phase retries inside
        // send_chat_request: gateways frequently drop long SSE streams
        // mid-body. A disconnect is only safe to retry when *nothing* was
        // delivered yet — once any text/tool/usage chunk reached the caller,
        // retrying would duplicate output, so the error propagates instead.
        let mut attempt = 0u32;
        loop {
            let response = self.send_request(&body).await?;

            let (tx, mut rx) = mpsc::channel(64);

            // Spawn a task that reads the SSE stream chunk-by-chunk and feeds
            // parsed Chunks into the channel. This gives us true streaming
            // instead of buffering the entire response body.
            tokio::spawn(async move {
                let mut sent_any = false;
                if let Err(e) = stream_sse_response(response, &tx, &mut sent_any).await {
                    let _ = tx.send(Err(e)).await;
                }
            });

            // Peek at the first item to decide retry vs. hand-off.
            match rx.recv().await {
                Some(Err(e)) if attempt < self.max_retries => {
                    let delay = crate::retry::backoff_duration(attempt + 1);
                    warn!(
                        "stream disconnected before any content, retry {}/{}: {e:?}",
                        attempt + 1,
                        self.max_retries
                    );
                    drop(rx);
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                Some(first) => {
                    // First item arrived — forward it, then the live rest.
                    let rest = tokio_stream::wrappers::ReceiverStream::new(rx);
                    let chained = tokio_stream::iter(std::iter::once(first)).chain(rest);
                    return Ok(Box::pin(chained));
                }
                None => {
                    return Err(DeepseeknovaError::provider(
                        "stream ended before producing any event",
                    ));
                }
            }
        }
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
    tx: &mpsc::Sender<Result<Chunk, DeepseeknovaError>>,
    sent_any: &mut bool,
) -> Result<(), DeepseeknovaError> {
    // Accumulate raw bytes per line to avoid UTF-8 corruption across TCP chunks
    let mut line_bytes: Vec<u8> = Vec::new();
    let mut tool_acc: Vec<AccToolCall> = Vec::new();

    let mut byte_stream = response.bytes_stream();

    while let Some(chunk_result) = byte_stream.next().await {
        let bytes = chunk_result.map_err(|e| {
            DeepseeknovaError::provider(format!("failed to read chunk from stream: {e}"))
        })?;

        for &b in bytes.iter() {
            match b {
                b'\n' => {
                    if line_bytes.is_empty() {
                        continue;
                    }
                    let line_str = String::from_utf8(line_bytes.clone()).map_err(|e| {
                        DeepseeknovaError::provider(format!("invalid UTF-8 in SSE stream: {e}"))
                    })?;
                    line_bytes.clear();
                    let trimmed = line_str.trim().to_string();
                    if trimmed.is_empty() {
                        continue;
                    }
                    process_sse_line(&trimmed, tx, &mut tool_acc, sent_any).await?;
                }
                b'\r' => { /* skip — handled by \n */ }
                _ => line_bytes.push(b),
            }
        }
    }

    // Process any remaining buffered data
    if !line_bytes.is_empty() {
        let tail_str = String::from_utf8(line_bytes).map_err(|e| {
            DeepseeknovaError::provider(format!("invalid UTF-8 in SSE stream tail: {e}"))
        })?;
        let trimmed = tail_str.trim().to_string();
        if !trimmed.is_empty() {
            process_sse_line(&trimmed, tx, &mut tool_acc, sent_any).await?;
        }
    }

    // Flush any pending tool calls
    flush_pending_tool_calls(tx, &mut tool_acc, sent_any).await?;

    Ok(())
}

/// Process a single SSE line (without the trailing \n).
async fn process_sse_line(
    line: &str,
    tx: &mpsc::Sender<Result<Chunk, DeepseeknovaError>>,
    tool_acc: &mut Vec<AccToolCall>,
    sent_any: &mut bool,
) -> Result<(), DeepseeknovaError> {
    // End-of-stream marker
    if line == "data: [DONE]" {
        flush_pending_tool_calls(tx, tool_acc, sent_any).await?;
        *sent_any = true;
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

    // Final usage chunk — map all DeepSeek-specific accounting (context-cache
    // hit/miss and reasoning tokens) via the shared conversion so the stream
    // never drops billed reasoning tokens.
    if let Some(ref u) = resp.usage {
        *sent_any = true;
        let _ = tx.send(Ok(Chunk::Usage(u.to_usage()))).await;
    }

    for choice in resp.choices {
        // Check for finish_reason = "tool_calls" to flush accumulated calls
        let is_tool_call_finish = choice.finish_reason.as_deref() == Some("tool_calls");

        if let Some(ref delta) = choice.delta {
            // --- Text content ---
            if let Some(ref content) = delta.content {
                if !content.is_empty() {
                    *sent_any = true;
                    let _ = tx.send(Ok(Chunk::TextDelta(content.clone()))).await;
                }
            }

            // --- Reasoning content (DeepSeek thinking mode) ---
            if let Some(ref reasoning) = delta.reasoning_content {
                if !reasoning.is_empty() {
                    *sent_any = true;
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
                                    *sent_any = true;
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
                                *sent_any = true;
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
            flush_pending_tool_calls(tx, tool_acc, sent_any).await?;
        }
    }

    Ok(())
}

/// Emit ToolCallEnd for any pending (accumulated but not flushed) tool calls.
async fn flush_pending_tool_calls(
    tx: &mpsc::Sender<Result<Chunk, DeepseeknovaError>>,
    tool_acc: &mut Vec<AccToolCall>,
    sent_any: &mut bool,
) -> Result<(), DeepseeknovaError> {
    for acc in tool_acc.drain(..) {
        if let (Some(id), Some(name)) = (acc.id, acc.name) {
            *sent_any = true;
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
    use crate::test_util::{clear_proxy_env, ENV_LOCK};
    use deepseeknova_core::tool::ToolContext;
    use deepseeknova_core::types::ToolSchema;

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

        async fn execute(
            &self,
            _ctx: &ToolContext,
            _args: &str,
        ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
            Ok("ok".into())
        }
    }

    fn user_msg(content: &str) -> Message {
        Message {
            role: deepseeknova_core::Role::User,
            content: content.to_string(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    /// A configured temperature must appear in the serialised request body.
    #[test]
    fn build_request_injects_temperature_when_set() {
        std::env::set_var("DPNOVA_TEMP_KEY", "sk-test");
        let provider = OpenAIProvider::new(
            "https://api.deepseek.com",
            "deepseek-v4-flash",
            "DPNOVA_TEMP_KEY",
            30,
            0,
        )
        .unwrap()
        .with_temperature(0.7);
        let msgs = [user_msg("hi")];
        let tools: Vec<&dyn Tool> = Vec::new();
        let body = provider.build_request(&msgs, &tools, false);
        let temp = body["temperature"].as_f64().unwrap();
        assert!(
            (temp - 0.7).abs() < 1e-6,
            "temperature must reach the request body, got {temp}"
        );
        std::env::remove_var("DPNOVA_TEMP_KEY");
    }

    /// Unset temperature must not be serialised at all.
    #[test]
    fn build_request_omits_temperature_when_unset() {
        std::env::set_var("DPNOVA_TEMP_KEY2", "sk-test");
        let provider = OpenAIProvider::new(
            "https://api.deepseek.com",
            "deepseek-v4-flash",
            "DPNOVA_TEMP_KEY2",
            30,
            0,
        )
        .unwrap();
        let msgs = [user_msg("hi")];
        let tools: Vec<&dyn Tool> = Vec::new();
        let body = provider.build_request(&msgs, &tools, false);
        assert!(
            body.get("temperature").is_none(),
            "unset temperature must be omitted"
        );
        std::env::remove_var("DPNOVA_TEMP_KEY2");
    }

    /// build_tools must produce one entry per tool (sorted), and return `None`
    /// for an empty set — the cache must not change the observable payload.
    #[test]
    fn build_tools_payload_is_correct_and_cached() {
        std::env::set_var("DPNOVA_CACHE_KEY", "sk-test");
        let provider = OpenAIProvider::new(
            "https://api.deepseek.com",
            "deepseek-v4-flash",
            "DPNOVA_CACHE_KEY",
            30,
            0,
        )
        .unwrap();
        let tool_a = NoopTool;
        let tool_b = NoopTool; // distinct object → distinct identity
        let set_a: Vec<&dyn Tool> = vec![&tool_a];
        let set_ab: Vec<&dyn Tool> = vec![&tool_a, &tool_b];

        let v_a = provider.build_tools(&set_a).expect("tools should be built");
        let v_ab = provider
            .build_tools(&set_ab)
            .expect("tools should be built");
        assert_eq!(v_a.as_array().unwrap().len(), 1);
        assert_eq!(v_ab.as_array().unwrap().len(), 2);
        assert_eq!(v_ab.as_array().unwrap()[0]["function"]["name"], "noop");
        assert!(v_a.as_array().unwrap()[0]["function"]["name"].is_string());

        let empty: Vec<&dyn Tool> = Vec::new();
        assert!(provider.build_tools(&empty).is_none(), "empty set → None");
        std::env::remove_var("DPNOVA_CACHE_KEY");
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
        let mut sent_any = false;

        for line in sse_data.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            process_sse_line(trimmed, &tx, &mut tool_acc, &mut sent_any)
                .await
                .unwrap();
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
        let mut sent_any = false;

        for line in sse_data.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            process_sse_line(trimmed, &tx, &mut tool_acc, &mut sent_any)
                .await
                .unwrap();
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
        let mut sent_any = false;

        for line in sse_data.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            process_sse_line(trimmed, &tx, &mut tool_acc, &mut sent_any)
                .await
                .unwrap();
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

    /// Verify the final DeepSeek usage frame is parsed into a Chunk::Usage that
    /// preserves context-cache hit/miss tokens AND the billed reasoning tokens
    /// nested under `completion_tokens_details`.
    #[tokio::test]
    async fn parse_sse_deepseek_usage_with_reasoning_tokens() {
        let sse_data = r#"data: {"choices":[{"index":0,"delta":{"content":"hi"}}]}

data: {"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":40,"total_tokens":140,"prompt_cache_hit_tokens":64,"prompt_cache_miss_tokens":36,"completion_tokens_details":{"reasoning_tokens":25}}}

data: [DONE]
"#;

        let (tx, mut rx) = mpsc::channel(64);
        let mut tool_acc = Vec::new();
        let mut sent_any = false;

        for line in sse_data.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            process_sse_line(trimmed, &tx, &mut tool_acc, &mut sent_any)
                .await
                .unwrap();
        }

        drop(tx);

        let mut chunks = Vec::new();
        while let Some(chunk) = rx.recv().await {
            chunks.push(chunk.unwrap());
        }

        let usage = chunks
            .iter()
            .find_map(|c| match c {
                Chunk::Usage(u) => Some(u),
                _ => None,
            })
            .expect("should emit a Chunk::Usage");

        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 40);
        assert_eq!(usage.total_tokens, 140);
        assert_eq!(usage.cache_hit_tokens, 64, "cache hit tokens must survive");
        assert_eq!(
            usage.cache_miss_tokens, 36,
            "cache miss tokens must survive"
        );
        assert_eq!(
            usage.reasoning_tokens, 25,
            "billed reasoning tokens from completion_tokens_details must survive"
        );
    }

    /// Usage frames without the nested `completion_tokens_details` object must
    /// degrade gracefully to zero reasoning tokens (non-reasoning models).
    #[test]
    fn response_usage_reasoning_tokens_defaults_to_zero() {
        let json = r#"{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}"#;
        let usage: crate::types::ResponseUsage = serde_json::from_str(json).unwrap();
        assert_eq!(usage.reasoning_tokens(), 0);
        let mapped = usage.to_usage();
        assert_eq!(mapped.reasoning_tokens, 0);
        assert_eq!(mapped.total_tokens, 15);
    }

    // env 串行化锁与代理清除函数见 crate::test_util（跨 openai/embeddings
    // 共享，避免 std::env 并发修改 UB）。

    /// Mock SSE 服务器：第 1 次请求返回 200 但 body 中途断开（模拟网关
    /// 断流），第 2 次返回完整 SSE。返回 base_url 与请求记录。
    fn serve_stream_retry() -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for i in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                loop {
                    let n = stream.read(&mut tmp).unwrap();
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                tx.send(String::from_utf8_lossy(&buf).to_string()).unwrap();
                if i == 0 {
                    // 声明 1024 字节但 body 一个字节都不写就断开 → 读 body 第一块即报错
                    let head =
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 1024\r\n\r\n";
                    let _ = stream.write_all(head.as_bytes());
                    drop(stream);
                } else {
                    let body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"}}]}\n\ndata: [DONE]\n\n";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    drop(stream);
                }
            }
        });
        (format!("http://{addr}/v1"), rx)
    }

    /// 流式响应在发出任何内容前断流时必须重试；重试后拿到完整输出。
    #[tokio::test]
    async fn stream_retries_zero_output_disconnect() {
        let _guard = ENV_LOCK.lock().await;
        clear_proxy_env();
        std::env::set_var("DEEPSEEK_API_KEY", "sk-test");

        let (base, rx) = serve_stream_retry();
        let provider =
            OpenAIProvider::new(&base, "deepseek-v4-flash", "DEEPSEEK_API_KEY", 30, 2).unwrap();

        let msg = Message {
            role: deepseeknova_core::Role::User,
            content: "hi".into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        };
        let tool_refs: Vec<&dyn Tool> = Vec::new();
        let messages = [msg];
        let validated = ValidatedRequest::new(&messages, &tool_refs).unwrap();

        let mut stream = provider.stream(validated).await.unwrap();
        let mut text = String::new();
        while let Some(item) = stream.next().await {
            if let Ok(Chunk::TextDelta(t)) = item {
                text.push_str(&t);
            }
        }

        assert_eq!(text, "hello", "重试后应拿到完整输出");
        assert_eq!(
            rx.try_iter().count(),
            2,
            "应发生 2 次 HTTP 请求（首次断流 + 重试）"
        );
        std::env::remove_var("DEEPSEEK_API_KEY");
    }

    /// 已发出过内容后断流不得重试（重试会重复输出）——错误直接上抛。
    #[tokio::test]
    async fn stream_does_not_retry_after_partial_output() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            // 读取请求头直到 \r\n\r\n
            loop {
                let n = stream.read(&mut tmp).unwrap();
                if n == 0 {
                    return;
                }
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            // 读完请求 body：reqwest 的 send() 会发送完整 POST body，若 mock
            // 在 body 未读完时就 drop 连接，reqwest 写 body 遇 RST 会导致
            // send_request 失败（而非仅 stream 阶段失败），污染重试判定。
            let header_end = buf
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|i| i + 4)
                .unwrap_or(buf.len());
            let content_length = String::from_utf8_lossy(&buf[..header_end])
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|v| v.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            let mut remaining = content_length.saturating_sub(buf.len() - header_end);
            while remaining > 0 {
                let n = stream.read(&mut tmp).unwrap_or(0);
                if n == 0 {
                    break;
                }
                remaining = remaining.saturating_sub(n);
            }
            // 用 chunked 编码：先发一个完整 chunk（partial 帧），再不带终止的
            // `0\r\n\r\n` 直接断开 → reqwest 先交付已解析的 chunk，再在期待
            // 下一个 chunk 时遇 EOF 报错。这样稳定触发"已产出内容后断开"，
            // 避免 Content-Length 未满时 reqwest 首块行为不确定导致的竞态。
            let body =
                b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}]}\n\n";
            let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";
            let chunk_header = format!("{:x}\r\n", body.len());
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(chunk_header.as_bytes());
            let _ = stream.write_all(body);
            let _ = stream.write_all(b"\r\n");
            drop(stream);
        });

        let _guard = ENV_LOCK.lock().await;
        clear_proxy_env();
        std::env::set_var("DEEPSEEK_API_KEY", "sk-test");
        let base = format!("http://{addr}/v1");
        let provider =
            OpenAIProvider::new(&base, "deepseek-v4-flash", "DEEPSEEK_API_KEY", 30, 2).unwrap();

        let msg = Message {
            role: deepseeknova_core::Role::User,
            content: "hi".into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        };
        let tool_refs: Vec<&dyn Tool> = Vec::new();
        let messages = [msg];
        let validated = ValidatedRequest::new(&messages, &tool_refs).unwrap();

        let mut stream = provider.stream(validated).await.unwrap();
        let mut saw_error = false;
        let mut saw_text = false;
        while let Some(item) = stream.next().await {
            match item {
                Ok(Chunk::TextDelta(t)) => saw_text |= t == "partial",
                Err(_) => saw_error = true,
                _ => {}
            }
        }
        assert!(saw_text, "应收到首个 data 帧");
        assert!(
            saw_error,
            "已产出内容后的断流必须上抛错误，不能重试导致重复输出"
        );
        std::env::remove_var("DEEPSEEK_API_KEY");
    }
}
