use crate::memory::Memory;
use deepseeknova_core::chunk::{Chunk, Usage};
use deepseeknova_core::tool::ToolContext;
use deepseeknova_core::types::{FunctionCall, ToolCall};
use deepseeknova_core::{
    Message, Role, RunEvent, RunEventStream, RunInput, RunOutput, Runner, Tool,
};
use deepseeknova_permission::{Decision, PermissionGate};
use deepseeknova_provider::Provider;
use deepseeknova_security::context::SecurityContext;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Approximate characters-per-token for rough heuristics.
const CHARS_PER_TOKEN: f32 = 4.0;

// Re-export the approval trait (defined in core, next to `RunEvent`) so
// existing `deepseeknova_agent::ApprovalResponder` references keep resolving.
pub use deepseeknova_core::runner::ApprovalResponder;

// ---------------------------------------------------------------------------
// Agent — the main agent runner
// ---------------------------------------------------------------------------

pub struct Agent {
    provider: Arc<dyn Provider>,
    tools: HashMap<String, Arc<dyn Tool>>,
    max_steps: usize,
    system_prompt: Option<String>,
    /// Workspace root used to confine filesystem tool calls. Defaults to the
    /// process working directory at construction time.
    workspace_root: PathBuf,
    /// Security context injected into every ToolContext. Defaults to the
    /// safe-defaults policy (all builtin capabilities granted).
    security: SecurityContext,

    compaction_threshold_tokens: Option<u32>,

    /// Optional persistent conversation store. When set, each `run_stream`
    /// seeds its working memory from this store at the start and writes the
    /// full conversation back at the end, giving the agent multi-turn memory
    /// across separate `run_stream` invocations. This is what enables desktop
    /// / CLI sessions to carry context — and crucially, it lets DeepSeek-V4's
    /// `reasoning_content` replay contract span user turns, not just the
    /// tool-loop within a single run.
    history: Option<Arc<tokio::sync::Mutex<Vec<Message>>>>,

    /// Optional permission gate. When set, every tool call is checked before
    /// execution: Allow → run, Deny → blocked, Ask → routed to the approval
    /// responder. When `None` (the default), tools run unconditionally.
    permission: Option<Arc<PermissionGate>>,

    /// Optional approval responder used to resolve `Ask` decisions.
    approval: Option<Arc<dyn ApprovalResponder>>,

    /// Build-time registered extensions injected into every ToolContext.
    /// Stored as closures to erase the concrete type while staying Clone-free.
    extensions: Vec<Arc<ExtensionApplier>>,

    /// Optional repo-map provider. When set, `run_stream` invokes it at the
    /// start of a fresh conversation to produce a code-graph "repo map" that is
    /// appended to the system prompt (stable prefix region). Returns `None`
    /// when no map is available (e.g. empty budget or index unavailable). This
    /// is wired by the runtime from a shared `GraphHandle` + token budget.
    repo_map_provider: Option<RepoMapProvider>,
}

/// Type-erased provider that yields the current repo-map text (or `None`).
pub type RepoMapProvider = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// Type-erased closure that inserts a build-time extension value into a
/// ToolContext's `ExtensionRegistry`.
type ExtensionApplier =
    dyn Fn(&mut deepseeknova_core::tool::ExtensionRegistry) + Send + Sync;

impl Agent {
    pub fn new(provider: Arc<dyn Provider>, max_steps: usize) -> Self {
        Self {
            provider,
            tools: HashMap::new(),
            max_steps: if max_steps == 0 { 10 } else { max_steps },
            system_prompt: None,
            workspace_root: std::env::current_dir().unwrap_or_default(),
            security: SecurityContext::with_safe_defaults(),

            compaction_threshold_tokens: None,
            history: None,
            permission: None,
            approval: None,
            extensions: Vec::new(),
            repo_map_provider: None,
        }
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    /// Append text to the system prompt (used to inject retrieval strategy hints).
    pub fn with_appended_system_prompt(mut self, extra: impl AsRef<str>) -> Self {
        match self.system_prompt {
            Some(ref mut s) => s.push_str(extra.as_ref()),
            None => self.system_prompt = Some(extra.as_ref().to_string()),
        }
        self
    }

    pub fn with_compaction_threshold(mut self, tokens: Option<u32>) -> Self {
        self.compaction_threshold_tokens = tokens;
        self
    }

    /// Attach a persistent conversation store so this agent carries memory
    /// across successive `run_stream` calls. Callers share one
    /// `Arc<Mutex<Vec<Message>>>` across turns (and reset it to start a new
    /// session). When the store is non-empty at run start, the system prompt
    /// is *not* re-injected — the prior turns already contain it.
    pub fn with_conversation_history(
        mut self,
        history: Arc<tokio::sync::Mutex<Vec<Message>>>,
    ) -> Self {
        self.history = Some(history);
        self
    }

    /// Override the workspace root used to confine filesystem tool calls.
    pub fn with_workspace_root(mut self, workspace_root: PathBuf) -> Self {
        self.workspace_root = workspace_root;
        self
    }

    /// Override the security context injected into every tool execution.
    pub fn with_security(mut self, security: SecurityContext) -> Self {
        self.security = security;
        self
    }

    /// Attach a permission gate. When set, tool calls are gated (Allow/Ask/Deny)
    /// before execution.
    pub fn with_permission_gate(mut self, gate: Arc<PermissionGate>) -> Self {
        self.permission = Some(gate);
        self
    }

    /// Attach an approval responder used to resolve `Ask` decisions from the
    /// permission gate.
    pub fn with_approval_responder(mut self, responder: Arc<dyn ApprovalResponder>) -> Self {
        self.approval = Some(responder);
        self
    }

    /// Register an arbitrary extension value that will be injected into the
    /// `ExtensionRegistry` of every tool execution context. Used e.g. to hand
    /// a shared code-graph index to graph tools.
    pub fn with_extension<T: std::any::Any + Send + Sync + Clone>(mut self, ext: T) -> Self {
        self.extensions
            .push(Arc::new(move |reg| reg.insert(ext.clone())));
        self
    }

    /// Attach a repo-map provider closure. At the start of a fresh conversation,
    /// the agent calls this to obtain a code-graph repo map that is appended to
    /// the system prompt (in the stable prefix region, preserving prefix cache
    /// semantics). Returning `None` skips injection.
    pub fn with_repo_map_provider(mut self, provider: RepoMapProvider) -> Self {
        self.repo_map_provider = Some(provider);
        self
    }

    pub fn register_tool(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.schema().name.clone();
        self.tools.insert(name, tool);
    }

    /// Names of all registered tools (for diagnostics/tests).
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// Build the ToolContext for a tool call, injecting security + all
    /// build-time registered extensions. Shared by run_stream and tests.
    #[allow(dead_code)] // exercised from tests today; Task 9/10 consume it in-crate
    pub(crate) fn make_tool_context(
        &self,
        call_id: &str,
        cancel: CancellationToken,
    ) -> ToolContext {
        build_tool_context(
            call_id,
            cancel,
            &self.workspace_root,
            &self.security,
            &self.extensions,
        )
    }
}

/// Shared ToolContext construction used by both `Agent::make_tool_context`
/// and the spawned agent loop (which cannot borrow `self`). Keeps the
/// injection set (workspace + security + registered extensions) in one place.
fn build_tool_context(
    call_id: &str,
    cancel: CancellationToken,
    workspace_root: &std::path::Path,
    security: &SecurityContext,
    extensions: &[Arc<ExtensionApplier>],
) -> ToolContext {
    let mut ctx = ToolContext::with_cancellation(call_id, cancel)
        .with_workspace(workspace_root.to_path_buf());
    ctx.extensions.insert(security.clone());
    for apply in extensions {
        apply(&mut ctx.extensions);
    }
    ctx
}

#[async_trait::async_trait]
impl Runner for Agent {
    async fn run_stream(&self, input: RunInput) -> anyhow::Result<RunEventStream> {
        let (tx, rx) = mpsc::channel(64);

        let provider = Arc::clone(&self.provider);
        let tools: Vec<Arc<dyn Tool>> = self.tools.values().cloned().collect();
        let max_steps = self.max_steps;
        let system_prompt = self.system_prompt.clone();
        let compaction_threshold = self.compaction_threshold_tokens;
        let workspace_root = self.workspace_root.clone();
        let security = self.security.clone();
        let history = self.history.clone();
        let permission = self.permission.clone();
        let approval = self.approval.clone();
        let extensions = self.extensions.clone();
        let repo_map_provider = self.repo_map_provider.clone();

        // Create a cancellation token and wire Ctrl-C (SIGINT) to cancel it.
        // This enables graceful interruption of the agent loop (e.g. Ctrl-C).
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {
                        info!("Ctrl-C received, cancelling agent...");
                        cancel_clone.cancel();
                        break;
                    }
                    _ = cancel_clone.cancelled() => break,
                }
            }
        });

        tokio::spawn(async move {
            let mut memory = Memory::new();

            // Seed working memory from the persistent conversation store, if
            // one is attached. This is what makes the agent remember prior
            // user turns (and preserves DeepSeek-V4 reasoning_content across
            // turns for the must_replay contract).
            let seeded = if let Some(ref hist) = history {
                let prior = hist.lock().await;
                for m in prior.iter() {
                    memory.add_message(m.clone());
                }
                !prior.is_empty()
            } else {
                false
            };

            // Inject the system prompt only on a fresh conversation. When the
            // store already holds prior turns, the system prompt is part of
            // them and re-injecting it would duplicate it.
            if !seeded {
                if let Some(ref sp) = system_prompt {
                    // Build the system prompt content, appending the code-graph
                    // repo map (if any) in the stable prefix region — after the
                    // base prompt, before the volatile conversation — mirroring
                    // context::PromptBuilder's Repo Map format so prefix-cache
                    // semantics hold.
                    // TODO(graph): personalized seeds from user input
                    let mut content = sp.clone();
                    if let Some(ref provider) = repo_map_provider {
                        if let Some(map) = provider() {
                            if !map.is_empty() {
                                content.push_str("\n\n---\n## Repo Map\n\n```\n");
                                content.push_str(&map);
                                content.push_str("\n```\n");
                            }
                        }
                    }
                    memory.add_message(Message {
                        role: Role::System,
                        content,
                        name: None,
                        tool_calls: None,
                        tool_call_id: None,
                        reasoning_content: None,
                    });
                }
            }

            let result = run_agent_loop(
                provider,
                tools,
                max_steps,
                compaction_threshold,
                &mut memory,
                input,
                &tx,
                &cancel,
                workspace_root,
                security,
                permission,
                approval,
                extensions,
            )
            .await;

            // Persist the full conversation back to the store so the next
            // run_stream call resumes with this context. We write back even
            // on error so partial progress (and any must_replay reasoning) is
            // not silently lost between turns.
            if let Some(ref hist) = history {
                let mut store = hist.lock().await;
                *store = memory.get_all();
            }

            if let Err(e) = result {
                warn!("agent loop error: {e}");
                let _ = tx.send(Err(e)).await;
            }
        });

        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }
}

// ---------------------------------------------------------------------------
// Agent loop — runs in a spawned task
// ---------------------------------------------------------------------------

/// Accumulated tool call from streaming chunks.
#[derive(Debug, Clone)]
struct PendingToolCall {
    id: String,
    name: String,
    arguments: String,
}

async fn run_agent_loop(
    provider: Arc<dyn Provider>,
    tools: Vec<Arc<dyn Tool>>,
    max_steps: usize,
    compaction_threshold: Option<u32>,
    memory: &mut Memory,
    input: RunInput,
    tx: &mpsc::Sender<anyhow::Result<RunEvent>>,
    cancel: &CancellationToken,
    workspace_root: PathBuf,
    security: SecurityContext,
    permission: Option<Arc<PermissionGate>>,
    approval: Option<Arc<dyn ApprovalResponder>>,
    extensions: Vec<Arc<ExtensionApplier>>,
) -> anyhow::Result<()> {
    // Add user prompt
    memory.add_message(Message {
        role: Role::User,
        content: input.prompt.clone(),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    });

    // Resource-limit accounting (from SecurityContext.limits). Enforced at
    // step boundaries so each turn stays atomic — preserving the DeepSeek
    // replay invariant (no dangling tool_calls without matching results).
    let run_started = std::time::Instant::now();
    let mut tool_calls_made: usize = 0;

    for step in 0..max_steps {
        // Check for cancellation between steps
        if cancel.is_cancelled() {
            tx.send(Ok(RunEvent::Done(RunOutput {
                text: String::new(),
                tool_calls: Vec::new(),
                usage: None,
            })))
            .await
            .ok();
            return Ok(());
        }

        // Resource limits: overall wall-clock deadline and tool-call budget.
        if run_started.elapsed() > security.limits.max_execution_time {
            warn!(
                "agent exceeded max_execution_time ({:?})",
                security.limits.max_execution_time
            );
            return Err(anyhow::anyhow!(
                "exceeded max execution time ({:?})",
                security.limits.max_execution_time
            ));
        }
        if tool_calls_made >= security.limits.max_tool_calls {
            warn!(
                "agent exceeded max_tool_calls ({})",
                security.limits.max_tool_calls
            );
            return Err(anyhow::anyhow!(
                "exceeded max tool calls ({})",
                security.limits.max_tool_calls
            ));
        }

        info!("agent step {}/{}", step + 1, max_steps);

        // Atomic Turn-end compaction
        if let Some(threshold) = compaction_threshold {
            let all_msgs = memory.get_all();
            let tokens = estimate_tokens(&all_msgs);

            if tokens > threshold {
                let before = tokens;
                memory.shrink_large_results(threshold as usize * 4);
                let after_shrink = estimate_tokens(&memory.get_all());

                info!("shrunk tool results: {} -> {} tokens", before, after_shrink);

                if after_shrink > threshold {
                    warn!("context still over threshold after shrinking tool results. sliding window...");
                    memory.slide_window();
                    let after_slide = estimate_tokens(&memory.get_all());
                    info!("slid window: {} -> {} tokens", after_shrink, after_slide);
                }
            }
        }

        // Build the tool index for execution
        let tool_map: HashMap<String, Arc<dyn Tool>> = tools
            .iter()
            .map(|t| (t.schema().name.clone(), Arc::clone(t)))
            .collect();

        // Stream from provider
        let step_result = stream_and_process_turn(
            &provider,
            &tools,
            &tool_map,
            memory,
            tx,
            cancel,
            &workspace_root,
            &security,
            &mut tool_calls_made,
            permission.as_ref(),
            approval.as_ref(),
            &extensions,
        )
        .await?;

        match step_result {
            StepOutcome::Complete(output) => {
                tx.send(Ok(RunEvent::Done(output))).await.ok();
                return Ok(());
            }
            StepOutcome::Continue => {
                // Tools were executed; loop continues
                continue;
            }
            StepOutcome::MaxSteps => {
                warn!("agent reached max steps ({max_steps})");
                return Err(anyhow::anyhow!(
                    "reached max steps ({max_steps}) without completing the task"
                ));
            }
        }
    }

    warn!("agent reached max steps ({max_steps})");
    Err(anyhow::anyhow!(
        "reached max steps ({max_steps}) without completing the task"
    ))
}

// ---------------------------------------------------------------------------
// Turn processing — one provider call + optional tool execution
// ---------------------------------------------------------------------------

enum StepOutcome {
    /// Agent produced final text output — done.
    Complete(RunOutput),
    /// Agent made tool calls — results added to memory, continue loop.
    Continue,
    /// Nothing was produced — max steps will be exhausted.
    MaxSteps,
}

async fn stream_and_process_turn(
    provider: &Arc<dyn Provider>,
    tools: &[Arc<dyn Tool>],
    tool_map: &HashMap<String, Arc<dyn Tool>>,
    memory: &mut Memory,
    tx: &mpsc::Sender<anyhow::Result<RunEvent>>,
    cancel: &CancellationToken,
    workspace_root: &std::path::Path,
    security: &SecurityContext,
    tool_calls_made: &mut usize,
    permission: Option<&Arc<PermissionGate>>,
    approval: Option<&Arc<dyn ApprovalResponder>>,
    extensions: &[Arc<ExtensionApplier>],
) -> anyhow::Result<StepOutcome> {
    // Build tool refs for provider
    let tool_refs: Vec<&dyn Tool> = tools.iter().map(|t| t.as_ref()).collect();
    let messages = memory.get_all();

    // DeepSeek V4 protocol — ValidatedRequest::new fails early with
    // structured violation list, preventing corrupt messages from
    // ever reaching the provider
    let validated = deepseeknova_provider::ValidatedRequest::new(&messages, &tool_refs).map_err(
        |violations| {
            for v in &violations {
                tracing::error!(?v, "replay invariant violation before provider call");
            }
            anyhow::anyhow!(
                "history replay invariant violated: {} violation(s) detected",
                violations.len()
            )
        },
    )?;

    let mut stream = provider.stream(validated).await?;

    let mut text_buf = String::new();
    let mut reasoning_buf = String::new();
    let mut usage: Option<Usage> = None;
    let mut pending_calls: Vec<PendingToolCall> = Vec::new();

    // Consume the stream
    while let Some(chunk_result) = stream.next().await {
        if cancel.is_cancelled() {
            return Ok(StepOutcome::Complete(RunOutput {
                text: text_buf,
                tool_calls: Vec::new(),
                usage: None,
            }));
        }

        let chunk = chunk_result?;
        match chunk {
            Chunk::TextDelta(delta) => {
                text_buf.push_str(&delta);
                tx.send(Ok(RunEvent::TextDelta(delta))).await.ok();
            }
            Chunk::ReasoningDelta { text, signature } => {
                reasoning_buf.push_str(&text);
                tx.send(Ok(RunEvent::ReasoningDelta { text, signature }))
                    .await
                    .ok();
            }
            Chunk::ToolCallStart { id, name } => {
                // Start accumulating a new tool call
                pending_calls.push(PendingToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: String::new(),
                });
                tx.send(Ok(RunEvent::ToolCallStart { id, name })).await.ok();
            }
            Chunk::ToolCallDelta { id, args_delta } => {
                // Accumulate arguments into the matching pending call
                if let Some(call) = pending_calls.iter_mut().find(|c| c.id == id) {
                    call.arguments.push_str(&args_delta);
                }
                tx.send(Ok(RunEvent::ToolCallDelta { id, args_delta }))
                    .await
                    .ok();
            }
            Chunk::ToolCallEnd {
                id,
                name,
                arguments,
            } => {
                // If we already accumulated from deltas, merge; otherwise use the complete args
                if let Some(call) = pending_calls.iter_mut().find(|c| c.id == id) {
                    if !arguments.is_empty() && call.arguments.is_empty() {
                        call.arguments = arguments.clone();
                    }
                } else {
                    pending_calls.push(PendingToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: arguments.clone(),
                    });
                }
                tx.send(Ok(RunEvent::ToolCallEnd {
                    id,
                    name,
                    arguments,
                }))
                .await
                .ok();
            }
            Chunk::Usage(u) => {
                tx.send(Ok(RunEvent::Usage(u.clone()))).await.ok();
                usage = Some(u);
            }
            Chunk::Done => {}
        }
    }

    // --- Determine what the model wants ---
    let has_text = !text_buf.is_empty();
    let has_tool_calls = !pending_calls.is_empty();

    // Case 1: Only text → final answer
    if has_text && !has_tool_calls {
        // Add assistant message to memory
        memory.add_message(Message {
            role: Role::Assistant,
            content: text_buf.clone(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: if reasoning_buf.is_empty() {
                None
            } else {
                Some(reasoning_buf.clone())
            },
        });

        let final_calls: Vec<ToolCall> = pending_calls
            .into_iter()
            .map(|c| ToolCall {
                id: c.id,
                ty: "function".to_string(),
                function: FunctionCall {
                    name: c.name,
                    arguments: c.arguments,
                },
            })
            .collect();

        return Ok(StepOutcome::Complete(RunOutput {
            text: text_buf,
            tool_calls: final_calls,
            usage,
        }));
    }

    // Case 2: Tool calls (with or without text)
    if has_tool_calls {
        tx.send(Ok(RunEvent::TurnComplete)).await.ok();

        // Add assistant message with tool_calls to memory
        let tool_calls_for_msg: Vec<ToolCall> = pending_calls
            .iter()
            .map(|c| ToolCall {
                id: c.id.clone(),
                ty: "function".to_string(),
                function: FunctionCall {
                    name: c.name.clone(),
                    arguments: c.arguments.clone(),
                },
            })
            .collect();

        memory.add_message(Message {
            role: Role::Assistant,
            content: text_buf.clone(),
            name: None,
            tool_calls: Some(tool_calls_for_msg),
            tool_call_id: None,
            reasoning_content: if reasoning_buf.is_empty() {
                None
            } else {
                Some(reasoning_buf.clone())
            },
        });

        // Execute each tool call
        for call in &pending_calls {
            if cancel.is_cancelled() {
                break;
            }

            // Permission gate (only when a gate is attached). Decide before
            // executing: Allow → run, Deny → block, Ask → prompt via responder.
            let gate_block: Option<String> = match permission {
                Some(gate) => {
                    let decision = match tool_map.get(&call.name) {
                        Some(tool) => gate.check(tool.as_ref(), &call.arguments),
                        None => Decision::Allow,
                    };
                    match decision {
                        Decision::Allow => None,
                        Decision::Deny => Some("blocked by permission policy".to_string()),
                        Decision::Ask => {
                            let approved = if let Some(responder) = approval {
                                let approval_id = format!("approval_{}", uuid::Uuid::new_v4());
                                tx.send(Ok(RunEvent::ApprovalRequest {
                                    id: approval_id.clone(),
                                    title: format!("Allow tool: {}", call.name),
                                    description: Some(call.arguments.clone()),
                                }))
                                .await
                                .ok();
                                // Block until the user answers, but never
                                // deadlock: cancellation resolves to a denial.
                                tokio::select! {
                                    ans = responder.request(
                                        &approval_id,
                                        &call.name,
                                        Some(&call.arguments),
                                    ) => ans,
                                    _ = cancel.cancelled() => false,
                                }
                            } else {
                                // No responder wired (CLI/tests) → allow, so
                                // non-interactive callers keep working.
                                true
                            };
                            if approved {
                                gate.cache_decision(&call.name, &call.arguments, Decision::Allow);
                                None
                            } else {
                                Some("denied by user".to_string())
                            }
                        }
                    }
                }
                None => None,
            };

            let result = if let Some(reason) = gate_block {
                format!("Error: tool '{}' {}", call.name, reason)
            } else {
                let ctx = build_tool_context(
                    &call.id,
                    cancel.child_token(),
                    workspace_root,
                    security,
                    extensions,
                );
                if let Some(tool) = tool_map.get(&call.name) {
                    info!(tool = %call.name, id = %call.id, "executing tool");
                    match tool.execute(&ctx, &call.arguments).await {
                        Ok(output) => output,
                        Err(e) => {
                            let err_str = format!("{e:#}");
                            // Truncate tool errors to avoid leaking file paths or data into context
                            let max_len = 500;
                            let truncated = if err_str.len() > max_len {
                                let end = err_str.floor_char_boundary(max_len);
                                format!(
                                    "{}... [truncated {} bytes]",
                                    &err_str[..end],
                                    err_str.len() - end
                                )
                            } else {
                                err_str
                            };
                            format!("Error: {truncated}")
                        }
                    }
                } else {
                    format!("Error: unknown tool '{}'", call.name)
                }
            };

            // Count this executed tool call against the budget, and cap its
            // output size to protect the context window (max_output_bytes).
            *tool_calls_made += 1;
            let max_out = security.limits.max_output_bytes as usize;
            let result = if result.len() > max_out {
                let end = result.floor_char_boundary(max_out);
                format!(
                    "{}... [truncated {} bytes]",
                    &result[..end],
                    result.len() - end
                )
            } else {
                result
            };

            // Send ToolResult event
            tx.send(Ok(RunEvent::ToolResult {
                call_id: call.id.clone(),
                result: result.clone(),
            }))
            .await
            .ok();

            // Add tool result to memory
            memory.add_message(Message {
                role: Role::Tool,
                content: result,
                name: None,
                tool_calls: None,
                tool_call_id: Some(call.id.clone()),
                reasoning_content: None,
            });
        }

        return Ok(StepOutcome::Continue);
    }

    // Case 3: No text, no tool calls — end of stream without meaningful output
    if usage.is_some() {
        // Usage only (some models send a final usage-only chunk after stream ends)
        // This means the model returned nothing — end the turn
        return Ok(StepOutcome::Complete(RunOutput {
            text: String::new(),
            tool_calls: Vec::new(),
            usage,
        }));
    }

    // Nothing produced at all
    warn!("step produced no output");
    Ok(StepOutcome::MaxSteps)
}

// ---------------------------------------------------------------------------
// Token estimation helpers (public for testing)
// ---------------------------------------------------------------------------

/// Rough token count estimate from message content length.
pub fn estimate_tokens(messages: &[Message]) -> u32 {
    let char_count: usize = messages
        .iter()
        .map(|m| m.content.len() + m.reasoning_content.as_ref().map(|r| r.len()).unwrap_or(0))
        .sum();
    (char_count as f32 / CHARS_PER_TOKEN).ceil() as u32
}

#[allow(dead_code)]
fn format_role(role: Role) -> &'static str {
    match role {
        Role::System => "System",
        Role::User => "User",
        Role::Assistant => "Assistant",
        Role::Tool => "Tool",
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::MockProvider;
    use deepseeknova_core::tool::ToolContext;
    use deepseeknova_core::types::ToolSchema;
    use std::sync::Arc;
    use tokio_stream::StreamExt;

    // -----------------------------------------------------------------------
    // Simple structure for testing: a fake tool that records invocations
    // -----------------------------------------------------------------------

    struct SpyTool {
        name: &'static str,
        result: String,
    }

    #[async_trait::async_trait]
    impl Tool for SpyTool {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: self.name.to_string(),
                description: "spy tool".to_string(),
                parameters: serde_json::json!({"type":"object","properties":{}}),
            }
        }
        fn read_only(&self) -> bool {
            true
        }
        async fn execute(&self, _ctx: &ToolContext, _args: &str) -> anyhow::Result<String> {
            Ok(self.result.clone())
        }
    }

    // -----------------------------------------------------------------------
    // Unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn token_estimate_zero_for_empty() {
        assert_eq!(estimate_tokens(&[]), 0);
    }

    #[test]
    fn token_estimate_scales_with_content() {
        let msgs = vec![Message {
            role: Role::User,
            content: "hello world, this is a test message".to_string(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }];
        let tokens = estimate_tokens(&msgs);
        assert!(tokens > 0);
        assert!(tokens < 100);
    }

    #[test]
    fn format_role_returns_correct_names() {
        assert_eq!(format_role(Role::User), "User");
        assert_eq!(format_role(Role::Assistant), "Assistant");
        assert_eq!(format_role(Role::System), "System");
        assert_eq!(format_role(Role::Tool), "Tool");
    }

    // -----------------------------------------------------------------------
    // Integration tests: Agent + MockProvider
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn agent_streams_text_from_provider() {
        let provider = Arc::new(MockProvider::text("hello from agent"));
        let agent = Agent::new(provider, 3);

        let input = RunInput {
            prompt: "say hi".into(),
            images: vec![],
            model_override: None,
        };

        let mut stream = agent.run_stream(input).await.unwrap();
        let mut text = String::new();
        let mut done = false;

        while let Some(event) = stream.next().await {
            match event.unwrap() {
                RunEvent::TextDelta(t) => text.push_str(&t),
                RunEvent::Done(_) => done = true,
                _ => {}
            }
        }

        assert_eq!(text, "hello from agent");
        assert!(done);
    }

    #[tokio::test]
    async fn agent_respects_max_steps() {
        let provider = Arc::new(MockProvider::text("done"));
        let agent = Agent::new(provider, 2);

        let input = RunInput {
            prompt: "do something".into(),
            images: vec![],
            model_override: None,
        };

        let mut stream = agent.run_stream(input).await.unwrap();
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.unwrap());
        }

        assert!(events.iter().any(|e| matches!(e, RunEvent::Done(_))));
    }

    #[tokio::test]
    async fn agent_empty_prompt_still_runs() {
        let provider = Arc::new(MockProvider::text("response to empty"));
        let agent = Agent::new(provider, 3);

        let input = RunInput {
            prompt: "".into(),
            images: vec![],
            model_override: None,
        };

        let mut stream = agent.run_stream(input).await.unwrap();
        let mut text = String::new();
        while let Some(event) = stream.next().await {
            if let Ok(RunEvent::TextDelta(t)) = event {
                text.push_str(&t);
            }
        }

        assert!(text.contains("response to empty"));
    }

    #[tokio::test]
    async fn agent_registers_and_uses_tools() {
        let provider = Arc::new(MockProvider::text("used tool"));
        let mut agent = Agent::new(provider, 3);
        agent.register_tool(Arc::new(SpyTool {
            name: "spy",
            result: "tool ran".into(),
        }));

        let input = RunInput {
            prompt: "use spy".into(),
            images: vec![],
            model_override: None,
        };

        let mut stream = agent.run_stream(input).await.unwrap();
        let mut text = String::new();
        while let Some(event) = stream.next().await {
            if let Ok(RunEvent::TextDelta(t)) = event {
                text.push_str(&t);
            }
        }

        assert!(!text.is_empty(), "agent should produce text output");
    }

    #[tokio::test]
    async fn agent_max_steps_zero_defaults_to_ten() {
        let provider = Arc::new(MockProvider::text("ok"));
        let agent = Agent::new(provider, 0);

        let input = RunInput {
            prompt: "test".into(),
            images: vec![],
            model_override: None,
        };

        let result = agent.run_stream(input).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn agent_system_prompt_injected() {
        let provider = Arc::new(MockProvider::text("got prompt"));
        let agent = Agent::new(provider, 3).with_system_prompt("you are a test bot");

        let input = RunInput {
            prompt: "who are you".into(),
            images: vec![],
            model_override: None,
        };

        let result = agent.run_stream(input).await;
        assert!(
            result.is_ok(),
            "agent with system prompt should run without error"
        );
    }

    #[tokio::test]
    async fn agent_repo_map_provider_injected_into_system_prompt() {
        // A shared persistent store lets us observe the system message that the
        // agent builds at run start (it is persisted verbatim on turn 1).
        let history = Arc::new(tokio::sync::Mutex::new(Vec::<Message>::new()));
        let provider = Arc::new(MockProvider::text("ok"));
        let repo_map: RepoMapProvider =
            Arc::new(|| Some("crates/x/src/a.rs:\n│ pub fn foo()\n⋮".to_string()));
        let agent = Agent::new(provider, 3)
            .with_system_prompt("you are a test bot")
            .with_repo_map_provider(repo_map)
            .with_conversation_history(history.clone());

        let mut stream = agent
            .run_stream(RunInput {
                prompt: "who are you".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        let store = history.lock().await;
        let sys = store
            .iter()
            .find(|m| m.role == Role::System)
            .expect("system message should be persisted");
        assert!(sys.content.contains("you are a test bot"));
        assert!(sys.content.contains("Repo Map"));
        assert!(sys.content.contains("pub fn foo()"));
    }

    #[tokio::test]
    async fn agent_compaction_threshold_triggers() {
        let provider = Arc::new(MockProvider::text("compacted"));
        let agent = Agent::new(provider, 3).with_compaction_threshold(Some(1));

        let input = RunInput {
            prompt: "a really long message that should trigger compaction".into(),
            images: vec![],
            model_override: None,
        };

        let result = agent.run_stream(input).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn agent_executes_tool_calls() {
        // Mock provider returns a tool call first, then text
        let spy = Arc::new(SpyTool {
            name: "spy",
            result: "tool executed!".into(),
        });
        // Turn 1: tool call -> agent executes it -> continues loop
        // Turn 2: final text -> agent completes
        let responses = vec![
            vec![
                Chunk::ToolCallStart {
                    id: "call_1".into(),
                    name: "spy".into(),
                },
                Chunk::ToolCallEnd {
                    id: "call_1".into(),
                    name: "spy".into(),
                    arguments: "{}".into(),
                },
                Chunk::Done,
            ],
            vec![
                Chunk::TextDelta("done after tool".into()),
                Chunk::Usage(Usage::default()),
                Chunk::Done,
            ],
        ];
        let provider = Arc::new(MockProvider::sequential(responses).with_tools(vec![spy]));
        let mut agent = Agent::new(provider, 5);
        agent.register_tool(Arc::new(SpyTool {
            name: "spy",
            result: "tool executed!".into(),
        }));

        let input = RunInput {
            prompt: "use the tool".into(),
            images: vec![],
            model_override: None,
        };

        let mut stream = agent.run_stream(input).await.unwrap();
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.unwrap());
        }

        // Should see ToolResult and eventually Done
        let has_tool_result = events
            .iter()
            .any(|e| matches!(e, RunEvent::ToolResult { .. }));
        let has_done = events.iter().any(|e| matches!(e, RunEvent::Done(_)));
        assert!(
            has_tool_result,
            "agent should execute tools and emit ToolResult"
        );
        assert!(has_done, "agent should eventually complete with Done");
    }

    #[tokio::test]
    async fn agent_conversation_history_persists_across_runs() {
        // A shared persistent store simulates one desktop/CLI session.
        let history = Arc::new(tokio::sync::Mutex::new(Vec::<Message>::new()));

        // --- Turn 1 ---
        let agent1 = Agent::new(Arc::new(MockProvider::text("first answer")), 3)
            .with_system_prompt("sys")
            .with_conversation_history(history.clone());
        let mut s1 = agent1
            .run_stream(RunInput {
                prompt: "hello".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        // Draining to None guarantees the spawned task finished its writeback
        // (tx is dropped only after the store is persisted).
        while s1.next().await.is_some() {}

        {
            let store = history.lock().await;
            assert!(
                store.iter().any(|m| m.role == Role::System),
                "system prompt should be persisted on the first turn"
            );
            assert!(
                store
                    .iter()
                    .any(|m| m.role == Role::User && m.content == "hello"),
                "first user turn should be persisted"
            );
            assert!(
                store.len() >= 3,
                "expected at least system + user + assistant, got {}",
                store.len()
            );
        }

        // --- Turn 2: a brand-new agent sharing the same history store ---
        let agent2 = Agent::new(Arc::new(MockProvider::text("second answer")), 3)
            .with_system_prompt("sys")
            .with_conversation_history(history.clone());
        let mut s2 = agent2
            .run_stream(RunInput {
                prompt: "again".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while s2.next().await.is_some() {}

        let store = history.lock().await;
        // Both user turns present => memory carried across separate runs.
        assert!(
            store
                .iter()
                .any(|m| m.role == Role::User && m.content == "hello"),
            "turn-1 user message must survive into turn 2"
        );
        assert!(
            store
                .iter()
                .any(|m| m.role == Role::User && m.content == "again"),
            "turn-2 user message must be present"
        );
        // System prompt must NOT be duplicated on the seeded second run.
        let system_count = store.iter().filter(|m| m.role == Role::System).count();
        assert_eq!(
            system_count, 1,
            "system prompt must not be re-injected on a seeded run"
        );
    }

    #[tokio::test]
    async fn agent_persists_reasoning_content_across_turns() {
        // Proves the DeepSeek-V4 adaptation: an assistant turn's
        // reasoning_content is written into the shared history store, so a
        // subsequent run can replay it (must_replay contract spanning turns).
        let history = Arc::new(tokio::sync::Mutex::new(Vec::<Message>::new()));

        let turn1 = vec![
            Chunk::ReasoningDelta {
                text: "let me think about q1".into(),
                signature: Some("sig-1".into()),
            },
            Chunk::TextDelta("the answer".into()),
            Chunk::Usage(Usage::default()),
            Chunk::Done,
        ];
        let agent = Agent::new(Arc::new(MockProvider::new(turn1)), 3)
            .with_conversation_history(history.clone());
        let mut s = agent
            .run_stream(RunInput {
                prompt: "q1".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while s.next().await.is_some() {}

        let store = history.lock().await;
        assert!(
            store.iter().any(|m| m.role == Role::Assistant
                && m.reasoning_content.as_deref() == Some("let me think about q1")),
            "assistant reasoning_content must persist into the shared history \
             so DeepSeek-V4 reasoning replay works across turns"
        );
    }

    // -----------------------------------------------------------------------
    // Permission gate (D3/D4): Deny blocks execution; no gate = unchanged.
    // -----------------------------------------------------------------------

    /// A writer tool that records whether it actually executed.
    struct RecordingTool {
        ran: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait::async_trait]
    impl Tool for RecordingTool {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: "danger".to_string(),
                description: "writer tool".to_string(),
                parameters: serde_json::json!({"type":"object","properties":{}}),
            }
        }
        fn read_only(&self) -> bool {
            false
        }
        async fn execute(&self, _ctx: &ToolContext, _args: &str) -> anyhow::Result<String> {
            self.ran.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok("danger executed".to_string())
        }
    }

    /// Two-turn script: call `danger`, then finish with text.
    fn call_danger_then_done() -> Vec<Vec<Chunk>> {
        vec![
            vec![
                Chunk::ToolCallStart {
                    id: "c1".into(),
                    name: "danger".into(),
                },
                Chunk::ToolCallEnd {
                    id: "c1".into(),
                    name: "danger".into(),
                    arguments: "{}".into(),
                },
                Chunk::Done,
            ],
            vec![
                Chunk::TextDelta("finished".into()),
                Chunk::Usage(Usage::default()),
                Chunk::Done,
            ],
        ]
    }

    #[tokio::test]
    async fn agent_permission_gate_denies_tool() {
        use deepseeknova_permission::{Decision, PermissionGate, Policy, Rule};
        use std::sync::atomic::Ordering;

        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let provider = Arc::new(MockProvider::sequential(call_danger_then_done()));
        let gate = Arc::new(PermissionGate::new(Policy {
            mode: Decision::Ask,
            allow: vec![],
            ask: vec![],
            deny: vec![Rule::new("danger")],
        }));
        let mut agent = Agent::new(provider, 5).with_permission_gate(gate);
        agent.register_tool(Arc::new(RecordingTool { ran: ran.clone() }));

        let mut stream = agent
            .run_stream(RunInput {
                prompt: "go".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        let mut tool_result = String::new();
        while let Some(ev) = stream.next().await {
            if let Ok(RunEvent::ToolResult { result, .. }) = ev {
                tool_result = result;
            }
        }

        assert!(
            !ran.load(Ordering::SeqCst),
            "a Denied tool must NOT execute"
        );
        assert!(
            tool_result.contains("blocked by permission policy"),
            "denied tool result should explain the block, got: {tool_result}"
        );
    }

    #[tokio::test]
    async fn agent_without_gate_executes_tool() {
        use std::sync::atomic::Ordering;

        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let provider = Arc::new(MockProvider::sequential(call_danger_then_done()));
        // No permission gate attached — behavior must be unchanged (tool runs).
        let mut agent = Agent::new(provider, 5);
        agent.register_tool(Arc::new(RecordingTool { ran: ran.clone() }));

        let mut stream = agent
            .run_stream(RunInput {
                prompt: "go".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        assert!(
            ran.load(Ordering::SeqCst),
            "without a gate the tool must execute (behavior unchanged)"
        );
    }

    // -----------------------------------------------------------------------
    // Extension injection hook (Task 8)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn injects_custom_extension_into_tool_context() {
        #[derive(Clone)]
        struct Marker(u32);
        struct ProbeTool;
        #[async_trait::async_trait]
        impl Tool for ProbeTool {
            fn schema(&self) -> ToolSchema {
                ToolSchema {
                    name: "probe".into(),
                    description: "d".into(),
                    parameters: serde_json::json!({"type":"object","properties":{}}),
                }
            }
            fn read_only(&self) -> bool {
                true
            }
            async fn execute(&self, ctx: &ToolContext, _args: &str) -> anyhow::Result<String> {
                let m = ctx.extensions.get::<Marker>().map(|m| m.0).unwrap_or(0);
                Ok(format!("marker={m}"))
            }
        }

        let agent =
            Agent::new(Arc::new(MockProvider::text("ok")), 3).with_extension(Marker(42));
        let ctx = agent.make_tool_context("call-1", CancellationToken::new());
        let out = ProbeTool.execute(&ctx, "{}").await.unwrap();
        assert_eq!(out, "marker=42");
    }

    #[test]
    fn tool_names_lists_registered() {
        let mut agent = Agent::new(Arc::new(MockProvider::text("ok")), 3);
        agent.register_tool(Arc::new(SpyTool {
            name: "probe2",
            result: "r".into(),
        }));
        assert!(agent.tool_names().iter().any(|n| n == "probe2"));
    }
}
