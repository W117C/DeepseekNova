//! Agent Client Protocol (ACP) v1 stdio server.
//!
//! This is a minimal but spec-shaped adapter: newline-delimited JSON-RPC 2.0
//! on stdin/stdout, supporting `initialize`, `session/new`, `session/prompt`,
//! `session/cancel` and `session/close`. Each session owns one agent runner
//! plus a shared conversation history so consecutive prompts build on prior
//! turns. Permission `Ask` decisions are denied (fail-closed) because this
//! transport does not implement the permission request RPC yet.

use deepseeknova_core::runner::{RunEvent, RunInput, Runner};
use deepseeknova_core::Message;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, Mutex};
use tokio_stream::StreamExt;

/// Builds the agent runner for a new ACP session.
///
/// `workspace_root` is the session's `cwd` from `session/new` and becomes the
/// filesystem confinement root. `history` is the shared multi-turn conversation
/// store the CLI wires into the agent before wrapping it as a [`Runner`].
pub type AcpRunnerFactory = Arc<
    dyn Fn(
            PathBuf,
            Arc<Mutex<Vec<Message>>>,
        ) -> Result<Arc<dyn Runner>, deepseeknova_core::DeepseeknovaError>
        + Send
        + Sync,
>;

struct AcpSession {
    runner: Arc<dyn Runner>,
    /// Kept alive for the lifetime of the session so multi-turn memory does
    /// not drop after the factory returns.
    _history: Arc<Mutex<Vec<Message>>>,
    /// The in-flight prompt, if any. Only one prompt may run per session at a
    /// time; cancellation takes this entry so exactly one side (the prompt task
    /// or the cancel handler) writes the final JSON-RPC response.
    active: Option<ActivePrompt>,
}

struct ActivePrompt {
    request_id: Value,
    handle: tokio::task::JoinHandle<()>,
}

struct AcpServer {
    factory: AcpRunnerFactory,
    sessions: Arc<Mutex<HashMap<String, AcpSession>>>,
    writer: mpsc::UnboundedSender<Value>,
}

/// Run the ACP server over this process's stdin/stdout.
pub async fn serve_acp(
    factory: AcpRunnerFactory,
) -> Result<(), deepseeknova_core::DeepseeknovaError> {
    run_acp_io(factory, tokio::io::stdin(), tokio::io::stdout()).await
}

/// Run the ACP server over arbitrary async reader/writer (used by tests with
/// in-memory duplex sockets).
pub async fn run_acp_io<R, W>(
    factory: AcpRunnerFactory,
    input: R,
    output: W,
) -> Result<(), deepseeknova_core::DeepseeknovaError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (writer, rx) = mpsc::unbounded_channel::<Value>();
    let write_task = tokio::spawn(write_loop(output, rx));
    let server = Arc::new(AcpServer {
        factory,
        sessions: Arc::new(Mutex::new(HashMap::new())),
        writer,
    });

    let mut lines = BufReader::new(input).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(msg) => msg,
            Err(e) => {
                server.send(json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": { "code": -32700, "message": format!("Parse error: {e}") },
                }));
                continue;
            }
        };
        server.handle(msg).await;
    }
    // Client closed stdin: cancel in-flight prompts, then drain the writer so
    // responses to the last requests are not lost when the runtime exits.
    server.shutdown().await;
    drop(server);
    let _ = write_task.await;
    Ok(())
}

async fn write_loop<W: AsyncWrite + Unpin + Send + 'static>(
    mut output: W,
    mut rx: mpsc::UnboundedReceiver<Value>,
) {
    while let Some(msg) = rx.recv().await {
        let mut line = serde_json::to_string(&msg).unwrap_or_default();
        line.push('\n');
        if output.write_all(line.as_bytes()).await.is_err() || output.flush().await.is_err() {
            break;
        }
    }
}

impl AcpServer {
    fn send(&self, msg: Value) {
        let _ = self.writer.send(msg);
    }

    async fn handle(self: &Arc<Self>, msg: Value) {
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            self.send(rpc_error(
                msg.get("id").cloned().unwrap_or(Value::Null),
                -32600,
                "Invalid Request",
            ));
            return;
        };
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        match method {
            "initialize" => self.respond(id, self.initialize(params).await).await,
            "session/new" => self.respond(id, self.new_session(params).await).await,
            "session/prompt" => {
                let request_id = id.unwrap_or(Value::Null);
                if let Err(error) = self.start_prompt(request_id.clone(), params).await {
                    self.send(json!({ "jsonrpc": "2.0", "id": request_id, "error": error }));
                }
            }
            "session/cancel" => {
                // Notification: no JSON-RPC response, but the in-flight
                // session/prompt must be answered with stopReason=cancelled.
                self.cancel(params).await;
            }
            "session/close" => self.respond(id, self.close(params).await).await,
            _ => {
                self.respond(
                    id,
                    Err(rpc_error_value(
                        -32601,
                        format!("Method not found: {method}"),
                    )),
                )
                .await;
            }
        }
    }

    async fn respond(&self, id: Option<Value>, result: Result<Value, Value>) {
        let id = id.unwrap_or(Value::Null);
        match result {
            Ok(result) => self.send(json!({ "jsonrpc": "2.0", "id": id, "result": result })),
            Err(error) => self.send(json!({ "jsonrpc": "2.0", "id": id, "error": error })),
        }
    }

    async fn initialize(&self, params: Value) -> Result<Value, Value> {
        // We only support protocol v1. If the client asks for a higher version,
        // respond with our latest supported version (1) per the negotiation
        // rules; the client decides whether to keep the connection.
        let _client_version = params
            .get("protocolVersion")
            .and_then(Value::as_u64)
            .unwrap_or(1);
        Ok(json!({
            "protocolVersion": 1,
            "agentCapabilities": {
                "loadSession": false,
                "promptCapabilities": {
                    "image": false,
                    "audio": false,
                    "embeddedContext": false,
                },
                "mcpCapabilities": {},
                "sessionCapabilities": { "close": {} },
            },
            "agentInfo": {
                "name": "deepseeknova",
                "title": "DeepseekNova",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "authMethods": [],
        }))
    }

    async fn new_session(&self, params: Value) -> Result<Value, Value> {
        let cwd = params
            .get("cwd")
            .and_then(Value::as_str)
            .ok_or_else(|| rpc_error_value(-32602, "cwd must be a string"))?;
        let workspace_root = PathBuf::from(cwd);
        if !workspace_root.is_absolute() {
            return Err(rpc_error_value(-32602, "cwd must be an absolute path"));
        }
        if !workspace_root.is_dir() {
            return Err(rpc_error_value(
                -32602,
                format!("cwd does not exist or is not a directory: {cwd}"),
            ));
        }

        // ACP allows mcpServers here; this adapter does not connect to them
        // yet, so log and continue with the built-in toolset.
        if let Some(servers) = params.get("mcpServers") {
            if !servers.as_array().map(|s| s.is_empty()).unwrap_or(false) {
                tracing::warn!(
                    "acp: session/new mcpServers are not supported yet; ignoring {} server(s)",
                    servers.as_array().map(|s| s.len()).unwrap_or(0)
                );
            }
        }

        let history = Arc::new(Mutex::new(Vec::<Message>::new()));
        let runner = (self.factory)(workspace_root, Arc::clone(&history))
            .map_err(|e| rpc_error_value(-32000, format!("failed to build agent: {e}")))?;
        let session_id = format!("sess_{}", uuid::Uuid::new_v4().simple());
        self.sessions.lock().await.insert(
            session_id.clone(),
            AcpSession {
                runner,
                _history: history,
                active: None,
            },
        );
        Ok(json!({ "sessionId": session_id }))
    }

    async fn start_prompt(&self, request_id: Value, params: Value) -> Result<(), Value> {
        let session_id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| rpc_error_value(-32602, "sessionId must be a string"))?;
        let prompt = params
            .get("prompt")
            .and_then(Value::as_array)
            .ok_or_else(|| rpc_error_value(-32602, "prompt must be an array of content blocks"))?;
        let text = extract_prompt_text(prompt).ok_or_else(|| {
            rpc_error_value(-32602, "prompt must contain at least one text block")
        })?;

        let mut sessions = self.sessions.lock().await;
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| rpc_error_value(-32002, "session not found"))?;
        if session.active.is_some() {
            return Err(rpc_error_value(
                -32000,
                "another prompt is already in progress for this session",
            ));
        }

        let runner = Arc::clone(&session.runner);
        let writer = self.writer.clone();
        let sessions_map = Arc::clone(&self.sessions);
        let sid = session_id.to_string();
        let message_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
        let request_id_for_spawn = request_id.clone();
        let handle = tokio::spawn(async move {
            run_prompt(
                runner,
                sid.clone(),
                request_id_for_spawn,
                text,
                message_id,
                writer,
                sessions_map,
            )
            .await;
        });
        session.active = Some(ActivePrompt { request_id, handle });
        Ok(())
    }

    async fn cancel(&self, params: Value) {
        let Some(session_id) = params.get("sessionId").and_then(Value::as_str) else {
            return;
        };
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get_mut(session_id) {
            if let Some(active) = session.active.take() {
                active.handle.abort();
                self.send(json!({
                    "jsonrpc": "2.0",
                    "id": active.request_id,
                    "result": { "stopReason": "cancelled" },
                }));
            }
        }
    }

    async fn close(&self, params: Value) -> Result<Value, Value> {
        let session_id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| rpc_error_value(-32602, "sessionId must be a string"))?;
        let mut sessions = self.sessions.lock().await;
        let Some(mut session) = sessions.remove(session_id) else {
            return Err(rpc_error_value(-32002, "session not found"));
        };
        if let Some(active) = session.active.take() {
            active.handle.abort();
            self.send(json!({
                "jsonrpc": "2.0",
                "id": active.request_id,
                "result": { "stopReason": "cancelled" },
            }));
        }
        Ok(json!({}))
    }

    /// Cancel all in-flight prompts and drop sessions. Used when the client
    /// closes the stdio channel; no cancelled response is sent because there
    /// is no one to receive it.
    async fn shutdown(&self) {
        let mut sessions = self.sessions.lock().await;
        for session in sessions.values_mut() {
            if let Some(active) = session.active.take() {
                active.handle.abort();
            }
        }
        sessions.clear();
    }
}

/// Concatenate text content blocks from a `session/prompt` message. Resource
/// links are not resolved (no fs capability advertised); anything else is
/// ignored. Returns `None` when there is no text at all.
fn extract_prompt_text(blocks: &[Value]) -> Option<String> {
    let mut parts = Vec::new();
    for block in blocks {
        if block.get("type").and_then(Value::as_str) == Some("text") {
            if let Some(text) = block.get("text").and_then(Value::as_str) {
                parts.push(text.to_string());
            }
        }
    }
    let text = parts.join("\n");
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

fn rpc_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": rpc_error_value(code, message) })
}

fn rpc_error_value(code: i64, message: impl Into<String>) -> Value {
    json!({ "code": code, "message": message.into() })
}

async fn run_prompt(
    runner: Arc<dyn Runner>,
    session_id: String,
    request_id: Value,
    prompt: String,
    message_id: String,
    writer: mpsc::UnboundedSender<Value>,
    sessions: Arc<Mutex<HashMap<String, AcpSession>>>,
) {
    let mut failure: Option<String> = None;
    let input = RunInput {
        prompt,
        images: Vec::new(),
        model_override: None,
    };

    match runner.run_stream(input).await {
        Ok(mut stream) => {
            while let Some(event) = stream.next().await {
                match event {
                    Ok(RunEvent::TextDelta(text)) => {
                        send_update(
                            &writer,
                            &session_id,
                            json!({
                                "sessionUpdate": "agent_message_chunk",
                                "messageId": message_id,
                                "content": { "type": "text", "text": text },
                            }),
                        );
                    }
                    Ok(RunEvent::ReasoningDelta { text, .. }) => {
                        send_update(
                            &writer,
                            &session_id,
                            json!({
                                "sessionUpdate": "agent_thought_chunk",
                                "messageId": message_id,
                                "content": { "type": "text", "text": text },
                            }),
                        );
                    }
                    Ok(RunEvent::ToolCallStart { id, name }) => {
                        send_update(
                            &writer,
                            &session_id,
                            json!({
                                "sessionUpdate": "tool_call",
                                "toolCallId": id,
                                "title": name,
                                "name": name,
                                "kind": "other",
                                "status": "pending",
                            }),
                        );
                    }
                    Ok(RunEvent::ToolCallEnd {
                        id,
                        name,
                        arguments,
                    }) => {
                        send_update(
                            &writer,
                            &session_id,
                            json!({
                                "sessionUpdate": "tool_call_update",
                                "toolCallId": id,
                                "name": name,
                                "status": "completed",
                                "content": [{
                                    "type": "content",
                                    "content": { "type": "text", "text": arguments },
                                }],
                            }),
                        );
                    }
                    Ok(RunEvent::ToolResult { call_id, result }) => {
                        send_update(
                            &writer,
                            &session_id,
                            json!({
                                "sessionUpdate": "tool_call_update",
                                "toolCallId": call_id,
                                "content": [{
                                    "type": "content",
                                    "content": { "type": "text", "text": result },
                                }],
                            }),
                        );
                    }
                    // Text deltas are streamed individually; ToolCallDelta is
                    // accumulated into ToolCallEnd. The remaining events
                    // (usage, done, paused, verification, ...) are protocol
                    // internals that ACP has no direct mapping for yet.
                    Ok(_) => {}
                    Err(e) => failure = Some(e.to_string()),
                }
            }
        }
        Err(e) => failure = Some(e.to_string()),
    }

    // Clear the active marker BEFORE sending the response so a concurrent
    // session/cancel cannot claim the turn and double-respond.
    if let Some(session) = sessions.lock().await.get_mut(&session_id) {
        session.active = None;
    }
    let response = match failure {
        Some(message) => json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "error": { "code": -32000, "message": message },
        }),
        None => json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": { "stopReason": "end_turn" },
        }),
    };
    let _ = writer.send(response);
}

fn send_update(writer: &mpsc::UnboundedSender<Value>, session_id: &str, update: Value) {
    let _ = writer.send(json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": { "sessionId": session_id, "update": update },
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_core::chunk::Usage;
    use deepseeknova_core::runner::{RunEventStream, RunOutput};
    use deepseeknova_core::types::ToolCall;
    use tokio::io::AsyncBufRead;

    fn fake_factory() -> AcpRunnerFactory {
        Arc::new(|_workspace_root, _history| Ok(Arc::new(FakeRunner) as Arc<dyn Runner>))
    }

    struct FakeRunner;

    #[async_trait::async_trait]
    impl Runner for FakeRunner {
        async fn run_stream(
            &self,
            input: RunInput,
        ) -> Result<RunEventStream, deepseeknova_core::DeepseeknovaError> {
            let prompt = input.prompt.clone();
            let events: Vec<Result<RunEvent, deepseeknova_core::DeepseeknovaError>> = vec![
                Ok(RunEvent::ReasoningDelta {
                    text: "thinking...".to_string(),
                    signature: None,
                }),
                Ok(RunEvent::TextDelta("Hello ".to_string())),
                Ok(RunEvent::TextDelta("ACP ".to_string())),
                Ok(RunEvent::TextDelta("world!".to_string())),
                Ok(RunEvent::Done(RunOutput {
                    text: format!("echo: {prompt}"),
                    tool_calls: Vec::<ToolCall>::new(),
                    usage: Some(Usage {
                        prompt_tokens: 1,
                        completion_tokens: 1,
                        cache_hit_tokens: 0,
                        cache_miss_tokens: 0,
                        reasoning_tokens: 0,
                        total_tokens: 2,
                    }),
                })),
            ];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn initialize_new_session_prompt_roundtrip(
    ) -> Result<(), deepseeknova_core::DeepseeknovaError> {
        let (client, server) = tokio::io::duplex(16 * 1024);
        let (client_read_half, mut client_write_half) = tokio::io::split(client);
        let (server_read_half, server_write_half) = tokio::io::split(server);
        let server_task = tokio::spawn(run_acp_io(
            fake_factory(),
            server_read_half,
            server_write_half,
        ));
        let mut client_reader = BufReader::new(client_read_half);

        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": 1, "clientCapabilities": {}, "clientInfo": {} },
        });
        client_write_half
            .write_all(format!("{}\n", serde_json::to_string(&req)?).as_bytes())
            .await?;
        let line = read_rpc_line(&mut client_reader).await?;
        assert_eq!(line["id"], 1);
        assert_eq!(line["result"]["protocolVersion"], 1);
        assert_eq!(line["result"]["agentInfo"]["name"], "deepseeknova");

        let req = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": { "cwd": std::env::current_dir()?.to_string_lossy() },
        });
        client_write_half
            .write_all(format!("{}\n", serde_json::to_string(&req)?).as_bytes())
            .await?;
        let line = read_rpc_line(&mut client_reader).await?;
        let session_id = line["result"]["sessionId"].as_str().unwrap().to_string();

        let req = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": "hi" }],
            },
        });
        client_write_half
            .write_all(format!("{}\n", serde_json::to_string(&req)?).as_bytes())
            .await?;

        let mut text = String::new();
        loop {
            let line = read_rpc_line(&mut client_reader).await?;
            if line
                .get("result")
                .is_some_and(|r| r.get("stopReason").is_some())
            {
                assert_eq!(line["result"]["stopReason"], "end_turn");
                break;
            }
            if line["method"] == "session/update" {
                let update = &line["params"]["update"];
                if update["sessionUpdate"] == "agent_message_chunk" {
                    text.push_str(update["content"]["text"].as_str().unwrap_or(""));
                }
            }
        }
        assert_eq!(text, "Hello ACP world!");

        let req = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "session/close",
            "params": { "sessionId": session_id },
        });
        client_write_half
            .write_all(format!("{}\n", serde_json::to_string(&req)?).as_bytes())
            .await?;
        let line = read_rpc_line(&mut client_reader).await?;
        assert_eq!(line["id"], 4);
        assert_eq!(line["result"], json!({}));

        drop(client_write_half);
        drop(client_reader);
        server_task
            .await
            .map_err(|e| deepseeknova_core::DeepseeknovaError::Runner(e.to_string()))??;
        Ok(())
    }

    async fn read_rpc_line<R: AsyncBufRead + Unpin>(
        reader: &mut R,
    ) -> Result<Value, deepseeknova_core::DeepseeknovaError> {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        Ok(serde_json::from_str(&line)?)
    }
}
