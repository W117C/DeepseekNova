use crate::agent_manifest::{AgentGateMode, AgentPermission};
use crate::memory::Memory;
use crate::mention::resolve_mention;
use crate::recursion::{DelegateDepth, DelegationSink};
use crate::task_spec::{InputValues, TaskSpec};
use deepseeknova_core::chunk::{Chunk, Usage};
use deepseeknova_core::{
    Message, Role, RunEvent, RunEventStream, RunInput, RunOutput, Runner, Tool, ToolContext,
};
use deepseeknova_provider::Provider;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Approximate characters-per-token for rough heuristics.
use crate::tokens::estimate_tokens;

/// 子代理递归深度上限默认值（根派发 depth 1；可再派 depth 2…直至上限）。
pub const DEFAULT_MAX_DEPTH: usize = 3;

/// per-agent 模型解析：把声明模型名解析为 provider 实例。由上层
/// （runtime / CLI）基于 provider 工厂实现；未装配时声明模型回退默认 provider。
pub trait ModelResolver: Send + Sync {
    fn resolve(&self, name: &str) -> Option<Arc<dyn Provider>>;
}

// ---------------------------------------------------------------------------
// SubAgentConfig — independent context for a single sub-agent type
// ---------------------------------------------------------------------------

/// Configuration for a named sub-agent. Each sub-agent has its own
/// system prompt, tool set, and execution parameters.
///
/// 任务书支持：`spec` 承载可参数化任务文本（`${{ inputs.x }}` 占位符）与 RULES
/// 约束；`config_inputs` 为配置层默认参数值。inputs 来源为 prompt 协议的
/// `input:<name>=<value>` 行（调用方传值优先）；渲染后 RULES 追加进 System
/// 消息、task 追加进 User 消息（goal 之后）。
///
/// 执行参数（`tools` / `max_steps`）与渲染描述（`spec.tools` / `spec.max_steps`）
/// 由 builder 同步维护；`with_task_spec` 只替换渲染用 spec，调用方需自行保证
/// 执行参数一致。
#[derive(Clone)]
pub struct SubAgentConfig {
    pub name: String,
    pub system_prompt: String,
    /// 任务书（渲染用：task/rules/inputs；tools/max_steps 仅作描述）。
    pub spec: TaskSpec,
    /// 配置层默认参数值，prompt 传值优先。
    pub config_inputs: InputValues,
    /// 父级冻结的 deny 规则渲染行（"禁止操作"清单，注入 system prompt）。
    /// 执行层由共享 PermissionGate 结构性冻结；此清单是 prompt 层防御，
    /// 让子代理模型在发起调用前就知道边界。
    frozen_denies: Vec<String>,
    /// 执行用工具集（完整工具对象）。
    tools: Vec<Arc<dyn Tool>>,
    /// 执行步数上限。
    max_steps: usize,
    /// per-agent 模型覆盖（经 [`ModelResolver`] 解析；None = 默认 provider）。
    pub model: Option<String>,
    /// per-agent 权限声明（gate 模式 + 能力白名单）。
    pub permission: AgentPermission,
}

impl fmt::Debug for SubAgentConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubAgentConfig")
            .field("name", &self.name)
            .field("system_prompt", &self.system_prompt)
            .field("tools_count", &self.tools.len())
            .field("max_steps", &self.max_steps)
            .finish()
    }
}

impl SubAgentConfig {
    pub fn new(name: impl Into<String>, system_prompt: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            name: name.clone(),
            system_prompt: system_prompt.into(),
            spec: TaskSpec::simple(name, "", Vec::new(), 10),
            config_inputs: InputValues::new(),
            frozen_denies: Vec::new(),
            tools: Vec::new(),
            max_steps: 10,
            model: None,
            permission: AgentPermission::new(),
        }
    }

    pub fn with_tools(mut self, tools: Vec<Arc<dyn Tool>>) -> Self {
        self.spec.tools = tools.iter().map(|t| t.schema().name.clone()).collect();
        self.tools = tools;
        self
    }

    pub fn with_max_steps(mut self, steps: usize) -> Self {
        self.spec.max_steps = if steps == 0 { 10 } else { steps };
        self.max_steps = self.spec.max_steps;
        self
    }

    /// 进入参数化路径：替换渲染用任务书（tools/max_steps 仅作描述，执行参数
    /// 仍由 `with_tools` / `with_max_steps` 控制）。
    pub fn with_task_spec(mut self, spec: TaskSpec) -> Self {
        self.spec = spec;
        self
    }

    /// 配置层默认参数值（prompt 传值优先，仅补缺）。
    pub fn with_config_inputs(mut self, inputs: InputValues) -> Self {
        self.config_inputs = inputs;
        self
    }

    /// 注入父级冻结的 deny 规则渲染行（prompt 层防御：模型发起调用前
    /// 即知晓禁止边界；执行层仍由共享 PermissionGate 强制）。
    pub fn with_frozen_denies(mut self, denies: Vec<String>) -> Self {
        self.frozen_denies = denies;
        self
    }

    /// per-agent 模型覆盖。经 [`ModelResolver`] 解析为 provider；未装配
    /// resolver 或解析失败时回退默认 provider（并 warn）。
    pub fn with_model(mut self, model: Option<String>) -> Self {
        self.model = model;
        self
    }

    /// per-agent 权限声明：gate 模式 + 能力白名单。
    pub fn with_permission(mut self, permission: AgentPermission) -> Self {
        self.permission = permission;
        self
    }
}

// ---------------------------------------------------------------------------
// SubAgentRunner — delegate dispatch with independent context
// ---------------------------------------------------------------------------

/// `SubAgentRunner` implements the `Runner` trait and dispatches tasks
/// to named sub-agents. Each sub-agent invocation gets an independent
/// memory context, system prompt, and tool set.
///
/// The runner accepts a `RunInput` whose `prompt` encodes the sub-agent
/// name and goal. The expected format is:
///
/// ```text
/// sub_agent:<name>
/// goal:<goal text>
/// ```
///
/// If no `sub_agent:` prefix is found, the runner dispatches to a
/// default sub-agent named `"default"` if one is registered.
pub struct SubAgentRunner {
    provider: Arc<dyn Provider>,
    /// Provider used for history compaction; falls back to `provider`.
    compact_provider: Option<Arc<dyn Provider>>,
    sub_agents: HashMap<String, SubAgentConfig>,
    default_sub_agent: Option<String>,
    compaction_threshold_tokens: Option<u32>,
    /// 执行层权限门：子代理工具调用在**执行前**强制检查。
    /// 无 gate 时与主 agent 的 `permissions.enabled=false` 语义一致：
    /// 不经过门控直接执行；需要 fail-closed 的调用方应显式挂 gate。
    permission: Option<Arc<deepseeknova_permission::PermissionGate>>,
    /// 工具执行上下文装配：安全上下文（shell/fs/web 工具强制依赖，
    /// 缺失时 `enforce_capability` 直接报错）与工作区根。
    security: Option<deepseeknova_security::context::SecurityContext>,
    workspace_root: std::path::PathBuf,
    /// 子代理递归深度上限（根派发 depth 1）。超深派发被拒绝。
    max_depth: usize,
    /// per-agent 模型解析器（声明 model 时把名字解析为 provider）。
    model_resolver: Option<Arc<dyn ModelResolver>>,
    /// 递归派发出口：供子代理把嵌套调用送回本 runner（由上层在 Arc 包装后
    /// 经 [`Self::set_delegation_sink`] 装配；未装配时子代理无法递归派发）。
    delegation_sink: Arc<std::sync::Mutex<Option<Arc<dyn DelegationSink>>>>,
}

impl SubAgentRunner {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self {
            provider,
            compact_provider: None,
            sub_agents: HashMap::new(),
            default_sub_agent: None,
            compaction_threshold_tokens: None,
            permission: None,
            security: None,
            workspace_root: std::env::current_dir().unwrap_or_default(),
            max_depth: DEFAULT_MAX_DEPTH,
            model_resolver: None,
            delegation_sink: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Register a sub-agent configuration.
    pub fn register(&mut self, config: SubAgentConfig) {
        self.sub_agents.insert(config.name.clone(), config);
    }

    /// Set the default sub-agent name used when no explicit sub-agent
    /// is specified in the input prompt.
    pub fn with_default(mut self, name: impl Into<String>) -> Self {
        self.default_sub_agent = Some(name.into());
        self
    }

    /// Set the compaction token threshold for all sub-agent contexts.
    pub fn with_compaction_threshold(mut self, tokens: u32) -> Self {
        self.compaction_threshold_tokens = Some(tokens);
        self
    }

    /// Use a dedicated provider (e.g. the `compact` model pointer) for
    /// history compaction instead of the main provider.
    pub fn with_compact_provider(mut self, provider: Arc<dyn Provider>) -> Self {
        self.compact_provider = Some(provider);
        self
    }

    /// 挂接执行层权限门。子代理工具调用执行前强制检查：
    /// - Deny → 回填阻断结果（含 reason）
    /// - Ask → 回填"需要审批"（子代理无用户审批通道，fail-closed）
    /// - 无 gate → 不经过门控（与主 agent 权限关闭语义一致）
    pub fn with_permission_gate(
        mut self,
        gate: Arc<deepseeknova_permission::PermissionGate>,
    ) -> Self {
        self.permission = Some(gate);
        self
    }

    /// 注入工具执行所需的 SecurityContext（shell/fs/web 工具强依赖，
    /// 缺失时 `enforce_capability` 直接报错，子代理工具面整体不可用）。
    pub fn with_security(
        mut self,
        security: deepseeknova_security::context::SecurityContext,
    ) -> Self {
        self.security = Some(security);
        self
    }

    /// 设置工作区根（路径类工具的相对路径解析基准）。
    pub fn with_workspace_root(mut self, root: impl Into<std::path::PathBuf>) -> Self {
        self.workspace_root = root.into();
        self
    }

    /// 设置子代理递归深度上限（默认 [`DEFAULT_MAX_DEPTH`]）。
    /// 根派发 depth 1；`max_depth = 0` 视作 1。
    pub fn with_max_depth(mut self, depth: usize) -> Self {
        self.max_depth = depth.max(1);
        self
    }

    /// 装配 per-agent 模型解析器（声明 model 的子代理经此取 provider）。
    pub fn with_model_resolver(mut self, resolver: Arc<dyn ModelResolver>) -> Self {
        self.model_resolver = Some(resolver);
        self
    }

    /// 装配递归派发出口（Arc 包装后回填本 runner 自身，使子代理可再派子代理）。
    /// 运行时在 `Arc::new(runner)` 之后调用：
    /// `runner.set_delegation_sink(runner.clone());`
    pub fn set_delegation_sink(&self, sink: Arc<dyn DelegationSink>) {
        *self.delegation_sink.lock().unwrap() = Some(sink);
    }

    /// Parse the input prompt to extract sub-agent name, goal, and input values.
    /// Returns (sub_agent_name, goal_text, input_values).
    ///
    /// Input lines must appear before the `goal:` line:
    /// `input:<name>=<value>`. Malformed lines (no `=`, empty name, or empty
    /// value) are ignored.
    fn parse_input(prompt: &str) -> (Option<String>, String, InputValues) {
        let mut sub_agent: Option<String> = None;
        let mut inputs: HashMap<String, String> = HashMap::new();
        let mut goal_start = 0usize;

        // 逐行累加字节偏移：`goal:` 命中时取其行首偏移，而非
        // `prompt.find("goal:")`（后者会命中 input 值里的 "goal:" 子串，
        // 导致 goal 文本被污染——Bugbot 审查 MEDIUM 修复）。
        let mut line_offset = 0usize;
        for line in prompt.lines() {
            let trimmed = line.trim();
            if let Some(name) = trimmed.strip_prefix("sub_agent:") {
                sub_agent = Some(name.trim().to_string());
            } else if let Some(kv) = trimmed.strip_prefix("input:") {
                if let Some((k, v)) = kv.split_once('=') {
                    let key = k.trim();
                    let value = v.trim();
                    if !key.is_empty() && !value.is_empty() {
                        inputs.insert(key.to_string(), value.to_string());
                    }
                }
            } else if trimmed.strip_prefix("goal:").is_some() {
                goal_start = line_offset;
                break;
            }
            line_offset += line.len() + 1; // 行内容 + 换行符
        }

        let goal = if goal_start > 0 {
            prompt[goal_start..].trim().to_string()
        } else {
            // If no structured format, use the full prompt as the goal
            prompt.to_string()
        };

        (sub_agent, goal, InputValues::from(inputs))
    }

    /// Resolve the sub-agent to use. Falls back to default or error.
    fn resolve_sub_agent(&self, name: Option<String>) -> anyhow::Result<&SubAgentConfig> {
        if let Some(ref n) = name {
            self.sub_agents
                .get(n)
                .ok_or_else(|| anyhow::anyhow!("unknown sub-agent: '{n}'"))
        } else if let Some(ref default) = self.default_sub_agent {
            self.sub_agents
                .get(default)
                .ok_or_else(|| anyhow::anyhow!("default sub-agent '{default}' not registered"))
        } else {
            anyhow::bail!(
                "no sub-agent specified and no default configured. \
                 Use 'sub_agent:<name>' in the prompt or register a default."
            )
        }
    }

    /// @-mention 解析：返回首个**已知**子代理引用（零个 → None）。
    /// 多个已知引用 → Err（消歧失败，提示用户一次引用一个）。
    fn resolve_mention(&self, prompt: &str) -> anyhow::Result<Option<String>> {
        let known: Vec<String> = self.sub_agents.keys().cloned().collect();
        Ok(resolve_mention(prompt, &known)?.map(|m| m.name))
    }

    /// 带深度派发（供递归 sink 使用）：检查深度上限 → 构造提示 → 收集最终文本。
    /// `depth` 为本次派发深度（根派发 1）；`depth > max_depth` 拒绝。
    pub async fn run_at_depth(
        &self,
        agent: &str,
        goal: &str,
        values: &InputValues,
        depth: usize,
    ) -> anyhow::Result<String> {
        let mut stream = self
            .dispatch_stream(
                Some(agent.to_string()),
                goal.to_string(),
                values.clone(),
                depth,
            )
            .await?;
        let mut text = String::new();
        while let Some(ev) = stream.next().await {
            match ev? {
                RunEvent::TextDelta(delta) => text.push_str(&delta),
                RunEvent::Done(out) if !out.text.is_empty() => {
                    text = out.text;
                    break;
                }
                _ => {}
            }
        }
        if text.is_empty() {
            anyhow::bail!("sub-agent '{agent}' produced no output");
        }
        Ok(text)
    }

    /// 核心派发：解析配置 → 渲染任务书 → 应用 per-agent 模型/权限 → spawn 循环。
    async fn dispatch_stream(
        &self,
        sub_agent_name: Option<String>,
        goal: String,
        parsed_inputs: InputValues,
        depth: usize,
    ) -> anyhow::Result<RunEventStream> {
        // 深度上限：超深拒绝（递归守门；根派发 depth 1）。
        if depth > self.max_depth {
            anyhow::bail!(
                "sub-agent recursion depth exceeded (max {}, depth requested: {depth})",
                self.max_depth
            );
        }
        let (tx, rx) = mpsc::channel(64);

        // Resolve sub-agent config
        let config = self.resolve_sub_agent(sub_agent_name)?;

        // 渲染任务书：prompt 传值优先，config 默认值仅补缺。
        // 渲染失败（如缺 required 输入）直接报错。
        let rendered = config
            .spec
            .render(&parsed_inputs.merged_with(&config.config_inputs))?;

        // per-agent 模型：声明 model 时经 resolver 解析；未声明/解析失败
        // 回退默认 provider。
        let provider = match &config.model {
            Some(m) => match self.model_resolver.as_ref().and_then(|r| r.resolve(m)) {
                Some(p) => p,
                None => {
                    warn!(
                        sub_agent = %config.name,
                        model = %m,
                        "model resolver unavailable for declared model, falling back to default provider"
                    );
                    Arc::clone(&self.provider)
                }
            },
            None => Arc::clone(&self.provider),
        };
        let compact_provider = self
            .compact_provider
            .as_ref()
            .map(Arc::clone)
            .unwrap_or_else(|| Arc::clone(&provider));

        // per-agent gate 模式：
        // - Inherit → 共享 gate
        // - None → 绕过 gate（工具直接执行）
        // - FailClosed → 共享 gate；无共享 gate 时在循环内拒绝一切工具
        let (permission, fail_closed) = match config.permission.gate {
            AgentGateMode::Inherit => (self.permission.clone(), false),
            AgentGateMode::None => (None, false),
            AgentGateMode::FailClosed => (self.permission.clone(), true),
        };

        // per-agent 能力白名单：声明非空 → 与基底安全上下文求交（裁剪能力）。
        let security = if config.permission.capabilities.is_empty() {
            self.security.clone()
        } else {
            self.security.as_ref().map(|base| {
                let declared: HashSet<_> = config
                    .permission
                    .capabilities
                    .iter()
                    .map(|c| c.to_capability())
                    .collect();
                deepseeknova_security::context::SecurityContext {
                    capabilities: base.capabilities.intersection(&declared).cloned().collect(),
                    ..base.clone()
                }
            })
        };

        // Clone what the spawned task needs
        let workspace_root = self.workspace_root.clone();
        let tools = config.tools.clone();
        let max_steps = config.max_steps;
        let delegation_sink = self.delegation_sink.lock().unwrap().clone();
        let mut system_prompt = config.system_prompt.clone();
        if !config.frozen_denies.is_empty() {
            system_prompt.push_str("\n\n## 禁止操作（父级冻结，不可执行）\n");
            system_prompt.push_str(&config.frozen_denies.join("\n"));
        }
        if !rendered.rules.is_empty() {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(&rendered.rules);
        }
        let mut task = goal.clone();
        if !rendered.task.is_empty() {
            task.push_str("\n\n");
            task.push_str(&rendered.task);
        }
        let compaction_threshold = self.compaction_threshold_tokens;

        // Each sub-agent invocation gets fully independent memory
        let mut memory = Memory::new();

        // Inject the sub-agent's own system prompt
        memory.add_message(Message {
            role: Role::System,
            content: system_prompt,
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });

        info!(
            sub_agent = %config.name,
            goal = %goal,
            max_steps = max_steps,
            depth = depth,
            "dispatching sub-agent"
        );

        let handle = tokio::spawn(async move {
            if let Err(e) = run_sub_agent_loop(
                provider,
                compact_provider,
                tools,
                max_steps,
                compaction_threshold,
                &mut memory,
                task,
                &tx,
                permission,
                security,
                workspace_root,
                depth,
                delegation_sink,
                fail_closed,
            )
            .await
            {
                warn!("sub-agent loop error: {e}");
                let _ = tx.send(Err(e)).await;
            }
        });

        Ok(Box::pin(AbortOnDropStream {
            inner: rx,
            handle: Some(handle),
        }))
    }
}

#[async_trait::async_trait]
impl DelegationSink for SubAgentRunner {
    async fn delegate(
        &self,
        agent: &str,
        goal: &str,
        values: &InputValues,
        depth: usize,
    ) -> anyhow::Result<String> {
        self.run_at_depth(agent, goal, values, depth).await
    }
}

#[async_trait::async_trait]
impl Runner for SubAgentRunner {
    async fn run_stream(&self, input: RunInput) -> anyhow::Result<RunEventStream> {
        // Parse input: extract sub-agent name, goal, and input values
        let (sub_agent_name, goal, parsed_inputs) = Self::parse_input(&input.prompt);

        // @-mention 回退：无结构化 `sub_agent:` 行时，识别已知 @引用触发调度。
        // 命中则目标为该子代理，goal 为完整 prompt（引用保留，供子代理获知
        // 被谁唤起）；零命中按默认行为（default 子代理或报错）。
        let (sub_agent_name, goal) = match sub_agent_name {
            Some(n) => (Some(n), goal),
            None => match self.resolve_mention(&input.prompt)? {
                Some(n) => (Some(n), input.prompt.clone()),
                None => (None, input.prompt.clone()),
            },
        };

        self.dispatch_stream(sub_agent_name, goal, parsed_inputs, 1)
            .await
    }
}

/// 子代理事件流包装：`tokio::spawn` 的子代理任务不随调用方 future 被丢弃而
/// 取消（timeout/提前返回会 drop 掉 `run_stream` 的 future，但 spawn 的任务
/// 会继续跑到 `max_steps`，导致重试并发 fan-out）。本包装在 stream 被 drop
/// 时 abort 子代理任务，阻断后台执行与重复副作用（Bugbot 审查 HIGH-2 修复）。
struct AbortOnDropStream {
    inner: mpsc::Receiver<anyhow::Result<RunEvent>>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl tokio_stream::Stream for AbortOnDropStream {
    type Item = anyhow::Result<RunEvent>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.inner.poll_recv(cx)
    }
}

impl Drop for AbortOnDropStream {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

// ---------------------------------------------------------------------------
// Sub-agent loop — runs in a spawned task with independent context
// ---------------------------------------------------------------------------

async fn run_sub_agent_loop(
    provider: Arc<dyn Provider>,
    compact_provider: Arc<dyn Provider>,
    tools: Vec<Arc<dyn Tool>>,
    max_steps: usize,
    compaction_threshold: Option<u32>,
    memory: &mut Memory,
    goal: String,
    tx: &mpsc::Sender<anyhow::Result<RunEvent>>,
    permission: Option<Arc<deepseeknova_permission::PermissionGate>>,
    security: Option<deepseeknova_security::context::SecurityContext>,
    workspace_root: std::path::PathBuf,
    depth: usize,
    delegation_sink: Option<Arc<dyn DelegationSink>>,
    fail_closed: bool,
) -> anyhow::Result<()> {
    let cancel = CancellationToken::new();

    // 构建工具执行上下文：工作区 + 安全上下文 + 递归深度 + 递归派发出口。
    // 递归深度扩展供 `RecursiveDelegateTool` 读取当前深度（再派时 +1）。
    let build_ctx = |call_id: &str| {
        let mut ctx = ToolContext::new(call_id.to_string()).with_workspace(workspace_root.clone());
        if let Some(sec) = &security {
            ctx.extensions.insert(sec.clone());
        }
        ctx.extensions.insert(DelegateDepth(depth));
        if let Some(sink) = &delegation_sink {
            ctx.extensions.insert(sink.clone());
        }
        ctx
    };

    // Add user goal as the first user message
    memory.add_message(Message {
        role: Role::User,
        content: goal.clone(),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    });

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

        info!("sub-agent step {}/{}", step + 1, max_steps);

        // A1 热路径：每步只取一次会话快照；压缩（会修改历史）在修改点
        // 重新快照，其余步内读取（token 估算 / provider 请求）复用同一
        // 快照，消除步内重复全量克隆。
        let mut messages = memory.get_all();

        // Compact if needed
        if let Some(threshold) = compaction_threshold {
            let tokens = estimate_tokens(&messages);
            if tokens > threshold {
                let before = tokens;
                match compact_with_provider(compact_provider.as_ref(), &messages).await {
                    Ok(digest) => {
                        memory.compact(digest, None);
                        // compact 重写了历史 → 重新快照，provider 必须看到压缩后。
                        messages = memory.get_all();
                        let after = estimate_tokens(&messages);
                        info!("compacted {before} → {after} tokens");
                    }
                    Err(e) => {
                        warn!("compaction failed: {e}, using simple fallback");
                        let msg_count = messages.len();
                        let digest = format!(
                            "Conversation summary ({msg_count} messages). Content truncated due to length."
                        );
                        memory.compact(digest, None);
                        messages = memory.get_all();
                    }
                }
            }
        }

        // Build tool refs for provider
        let tool_refs: Vec<&dyn Tool> = tools.iter().map(|t| t.as_ref()).collect();

        // DeepSeek V4 protocol — ValidatedRequest::new fails early with
        // structured violations instead of corrupting provider state
        let validated = deepseeknova_provider::ValidatedRequest::new(&messages, &tool_refs)
            .map_err(|violations| {
                for v in &violations {
                    tracing::error!(?v, "replay invariant violation in sub-agent");
                }
                anyhow::anyhow!(
                    "history replay invariant violated in sub-agent: {} violation(s)",
                    violations.len()
                )
            })?;

        // Stream from provider
        let mut stream = provider.stream(validated).await?;
        let mut text_buf = String::new();
        let mut reasoning_buf = String::new();
        let mut usage: Option<Usage> = None;
        let mut tool_calls: Vec<(String, String, String)> = Vec::new();

        while let Some(chunk) = stream.next().await {
            match chunk? {
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
                    tx.send(Ok(RunEvent::ToolCallStart { id, name })).await.ok();
                }
                Chunk::ToolCallDelta { id, args_delta } => {
                    tx.send(Ok(RunEvent::ToolCallDelta { id, args_delta }))
                        .await
                        .ok();
                }
                Chunk::ToolCallEnd {
                    id,
                    name,
                    arguments,
                } => {
                    tool_calls.push((id.clone(), name.clone(), arguments.clone()));
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

        tx.send(Ok(RunEvent::TurnComplete)).await.ok();

        // ── 工具执行段（C2 修复）：模型产出的工具调用在子代理内执行 ──
        // 之前此段缺失：工具调用被透传后丢弃，子代理陷入
        // "产出调用→无结果回填→重复产出"直到 max_steps。
        //
        // 权限强制（有 gate 时执行前检查）：
        // - 无 gate → 直接执行（与主 agent 的 permissions.enabled=false
        //   语义一致；需要 fail-closed 的调用方应显式挂 gate）
        // - Deny → 回填阻断原因
        // - Ask → 回填"需要审批"（子代理无用户审批通道，不静默放行写工具）
        //
        // R2 修复：先写 assistant(tool_calls) 消息再回填 Tool 结果——
        // replay 校验把"无主 tool_call_id 的 Tool 消息"判为 OrphanToolResult，
        // 不写 assistant 消息会导致子代理任何工具调用后硬失败。
        if !tool_calls.is_empty() {
            // 1) assistant 消息（携带本轮全部 tool_calls，保住 replay 不变量）
            let calls_for_msg: Vec<deepseeknova_core::types::ToolCall> = tool_calls
                .iter()
                .map(|(id, name, arguments)| deepseeknova_core::types::ToolCall {
                    id: id.clone(),
                    ty: "function".to_string(),
                    function: deepseeknova_core::types::FunctionCall {
                        name: name.clone(),
                        arguments: arguments.clone(),
                    },
                })
                .collect();
            memory.add_message(Message {
                role: Role::Assistant,
                content: text_buf.clone(),
                name: None,
                tool_calls: Some(calls_for_msg),
                tool_call_id: None,
                reasoning_content: if reasoning_buf.is_empty() {
                    None
                } else {
                    Some(reasoning_buf.clone())
                },
            });

            // 2) 逐个执行（gate 检查 → 执行/阻断），回填 Tool 消息
            for (id, name, arguments) in &tool_calls {
                let result: String = match tools.iter().find(|t| &t.schema().name == name) {
                    None => format!("Error: unknown tool '{name}'"),
                    Some(tool) => {
                        let verdict = permission
                            .as_ref()
                            .map(|g| g.check(tool.as_ref(), arguments));
                        match verdict {
                            // 无共享 gate：默认直接执行；fail_closed 且无 gate →
                            // 拒绝一切工具（不静默放行）。
                            None if !fail_closed => {
                                let ctx = build_ctx(id);
                                match tool.execute(&ctx, arguments).await {
                                    Ok(out) => out,
                                    Err(e) => format!("Error: {e}"),
                                }
                            }
                            None => format!(
                                "Error: tool '{name}' blocked by permission policy: sub-agent is fail-closed and no permission gate is configured"
                            ),
                            Some(v) => match v.decision() {
                                deepseeknova_permission::Decision::Allow => {
                                    let ctx = build_ctx(id);
                                    match tool.execute(&ctx, arguments).await {
                                        Ok(out) => out,
                                        Err(e) => format!("Error: {e}"),
                                    }
                                }
                                deepseeknova_permission::Decision::Deny => {
                                    let mut msg = format!(
                                        "Error: tool '{name}' blocked by permission policy: {}",
                                        v.reason()
                                    );
                                    let sug: Vec<String> = v
                                        .suggestions()
                                        .iter()
                                        .map(|s| match s.rule.subject {
                                            Some(ref sub) => format!(
                                                "behavior={:?} rule={} subject={sub}",
                                                s.behavior, s.rule.tool
                                            ),
                                            None => format!(
                                                "behavior={:?} rule={}",
                                                s.behavior, s.rule.tool
                                            ),
                                        })
                                        .collect();
                                    if !sug.is_empty() {
                                        msg.push_str(&format!(
                                            "\n[建议] 添加规则即可自动放行: {}",
                                            sug.join("; ")
                                        ));
                                    }
                                    msg
                                }
                                deepseeknova_permission::Decision::Ask => format!(
                                    "Error: tool '{name}' requires approval (sub-agent has no approval channel; treat as denied)"
                                ),
                            },
                        }
                    }
                };
                memory.add_message(Message {
                    role: Role::Tool,
                    content: result,
                    name: None,
                    tool_calls: None,
                    tool_call_id: Some(id.clone()),
                    reasoning_content: None,
                });
            }
            // 工具轮次后继续下一循环（模型将基于工具结果继续）
            continue;
        }

        // If the model returned text (not tool calls), we are done
        if !text_buf.is_empty() && usage.is_some() {
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

            // 输出净化：中和权限修改指令形状，防父上下文被注入
            let sanitized = deepseeknova_security::sanitize::sanitize_output(&text_buf);
            if sanitized != text_buf {
                warn!("sub-agent output sanitized: permission-override shape(s) neutralized");
            }
            let output = RunOutput {
                text: sanitized,
                tool_calls: Vec::new(),
                usage,
            };
            tx.send(Ok(RunEvent::Done(output))).await.ok();
            return Ok(());
        }

        // If no text was produced, something went wrong
        if text_buf.is_empty() && usage.is_none() {
            warn!("step {step} produced no output");
            break;
        }

        // Add partial text to memory and continue loop
        memory.add_message(Message {
            role: Role::Assistant,
            content: text_buf,
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: if reasoning_buf.is_empty() {
                None
            } else {
                Some(reasoning_buf.clone())
            },
        });
    }

    warn!("sub-agent reached max steps ({max_steps})");
    Err(anyhow::anyhow!(
        "sub-agent reached max steps ({max_steps}) without completing the task"
    ))
}

/// Build a compaction digest by asking the provider to summarize old messages.
async fn compact_with_provider(
    provider: &dyn Provider,
    messages: &[Message],
) -> anyhow::Result<String> {
    let conversation_text: String = messages
        .iter()
        .map(|m| format!("[{}]: {}", format_role(m.role.clone()), m.content))
        .collect::<Vec<_>>()
        .join("\n\n");

    let summary_prompt = format!(
        "Summarize the following conversation into a concise digest. \
         Keep key decisions, action items, and context. \
         The summary will replace these messages to save context space.\n\n\
         <conversation>
{conversation_text}
</conversation>

\
         Provide a compact summary (under 500 words)."
    );

    let summary_msgs = vec![Message {
        role: Role::User,
        content: summary_prompt,
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    }];

    let validated = deepseeknova_provider::ValidatedRequest::new(&summary_msgs, &[])
        .map_err(|v| anyhow::anyhow!("invariant violation in sub-agent summarize: {:?}", v))?;
    let mut stream = provider.stream(validated).await?;
    let mut out = String::new();
    while let Some(chunk) = stream.next().await {
        if let Chunk::TextDelta(t) = chunk? {
            out.push_str(&t);
        }
    }
    Ok(out)
}

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

    // --- SubAgentConfig tests ---

    #[test]
    fn config_default_max_steps() {
        let config = SubAgentConfig::new("test", "you are a test agent");
        assert_eq!(config.max_steps, 10);
        assert_eq!(config.name, "test");
        assert!(config.tools.is_empty());
    }

    #[test]
    fn config_custom_max_steps() {
        let config = SubAgentConfig::new("test", "prompt").with_max_steps(5);
        assert_eq!(config.max_steps, 5);
    }

    #[test]
    fn config_zero_max_steps_clamped_to_10() {
        let config = SubAgentConfig::new("test", "prompt").with_max_steps(0);
        assert_eq!(config.max_steps, 10);
    }

    #[test]
    fn config_with_tools() {
        use deepseeknova_core::{Tool, ToolContext};
        use serde_json::json;

        struct DummyTool;
        #[async_trait::async_trait]
        impl Tool for DummyTool {
            fn schema(&self) -> deepseeknova_core::ToolSchema {
                deepseeknova_core::ToolSchema {
                    name: "dummy".to_string(),
                    description: "a dummy tool".to_string(),
                    parameters: json!({}),
                }
            }
            async fn execute(&self, _ctx: &ToolContext, _args: &str) -> anyhow::Result<String> {
                Ok("done".to_string())
            }
        }

        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(DummyTool)];
        let config = SubAgentConfig::new("test", "prompt").with_tools(tools);
        assert_eq!(config.tools.len(), 1);
        assert_eq!(config.tools[0].schema().name, "dummy");
    }

    // --- Input parsing tests ---

    #[test]
    fn parse_input_structured() {
        let prompt = "sub_agent:researcher\ngoal:find all Rust files";
        let (name, goal, inputs) = SubAgentRunner::parse_input(prompt);
        assert_eq!(name, Some("researcher".to_string()));
        assert_eq!(goal, "goal:find all Rust files");
        assert!(inputs.is_empty());
    }

    #[test]
    fn parse_input_just_goal() {
        let prompt = "goal:analyze this codebase";
        let (name, goal, _) = SubAgentRunner::parse_input(prompt);
        assert_eq!(name, None);
        assert_eq!(goal, "goal:analyze this codebase");
    }

    #[test]
    fn parse_input_plain_text() {
        let prompt = "just a plain prompt with no structure";
        let (name, goal, _) = SubAgentRunner::parse_input(prompt);
        assert_eq!(name, None);
        assert_eq!(goal, prompt);
    }

    #[test]
    fn parse_input_only_sub_agent() {
        let prompt = "sub_agent:reviewer\nsome free text here";
        let (name, goal, _) = SubAgentRunner::parse_input(prompt);
        assert_eq!(name, Some("reviewer".to_string()));
        assert_eq!(goal, prompt);
    }

    #[test]
    fn parse_input_whitespace_handling() {
        let prompt = "sub_agent:  security-auditor  \ngoal:  scan for vulnerabilities  ";
        let (name, goal, _) = SubAgentRunner::parse_input(prompt);
        assert_eq!(name, Some("security-auditor".to_string()));
        assert!(goal.starts_with("goal:"));
    }

    #[test]
    fn parse_input_with_values() {
        let prompt =
            "sub_agent:reviewer\ninput:path=src/lib.rs\ninput:depth=3\ngoal:review the change";
        let (name, goal, inputs) = SubAgentRunner::parse_input(prompt);
        assert_eq!(name, Some("reviewer".to_string()));
        assert!(goal.starts_with("goal:review the change"));
        assert_eq!(inputs.get("path"), Some("src/lib.rs"));
        assert_eq!(inputs.get("depth"), Some("3"));
    }

    #[test]
    fn parse_input_ignores_malformed_value_lines() {
        let prompt =
            "sub_agent:reviewer\ninput:no-equals\ninput:=empty-key\ninput:trailing=\ngoal:x";
        let (_, _, inputs) = SubAgentRunner::parse_input(prompt);
        assert!(inputs.is_empty());
    }

    #[test]
    fn parse_input_value_after_goal_is_goal_text() {
        // `input:` 行必须在 `goal:` 行之前；之后的行属于 goal 文本。
        let prompt = "sub_agent:reviewer\ngoal:see input:path=x";
        let (_, goal, inputs) = SubAgentRunner::parse_input(prompt);
        assert!(goal.contains("input:path=x"));
        assert!(inputs.is_empty());
    }

    // --- SubAgentRunner registration tests ---

    /// A minimal mock Provider for unit tests that don't exercise the agent loop.
    struct MockProvider;
    #[async_trait::async_trait]
    impl Provider for MockProvider {
        async fn generate(
            &self,
            _validated: deepseeknova_provider::ValidatedRequest<'_>,
        ) -> anyhow::Result<Message> {
            Ok(Message {
                role: Role::Assistant,
                content: "mock".to_string(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            })
        }
    }

    #[test]
    fn resolve_by_name() {
        let provider = Arc::new(MockProvider);
        let mut runner = SubAgentRunner::new(provider);
        runner.register(SubAgentConfig::new("coder", "you are a coder"));
        runner.register(SubAgentConfig::new("reviewer", "you are a reviewer"));

        let resolved = runner.resolve_sub_agent(Some("coder".to_string())).unwrap();
        assert_eq!(resolved.name, "coder");
        assert_eq!(resolved.system_prompt, "you are a coder");
    }

    #[test]
    fn resolve_unknown_errors() {
        let provider = Arc::new(MockProvider);
        let runner = SubAgentRunner::new(provider);

        let err = runner
            .resolve_sub_agent(Some("nonexistent".to_string()))
            .unwrap_err();
        assert!(err.to_string().contains("unknown sub-agent"));
    }

    #[test]
    fn resolve_falls_back_to_default() {
        let provider = Arc::new(MockProvider);
        let mut runner = SubAgentRunner::new(provider).with_default("orchestrator");
        runner.register(SubAgentConfig::new("orchestrator", "you orchestrate"));
        runner.register(SubAgentConfig::new("worker", "you do work"));

        // No explicit sub-agent -> uses default
        let resolved = runner.resolve_sub_agent(None).unwrap();
        assert_eq!(resolved.name, "orchestrator");
    }

    #[test]
    fn resolve_no_default_errors() {
        let provider = Arc::new(MockProvider);
        let runner = SubAgentRunner::new(provider);

        let err = runner.resolve_sub_agent(None).unwrap_err();
        assert!(err.to_string().contains("no sub-agent specified"));
    }

    // --- Token estimation tests ---

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

    // --- Compaction provider routing tests ---

    #[tokio::test]
    async fn compaction_uses_compact_provider() {
        use crate::test_utils::MockProvider;

        // 主 provider 回复正常文本；compact provider 回复特征摘要文本
        let main = Arc::new(MockProvider::text("main-answer"));
        let compact = Arc::new(MockProvider::text("COMPACT-DIGEST"));

        let mut runner = SubAgentRunner::new(main.clone())
            .with_compact_provider(compact.clone() as Arc<dyn Provider>)
            .with_compaction_threshold(1); // 阈值 1 token → 必触发压缩
        runner.register(SubAgentConfig::new("t", "you are t"));
        let runner = runner.with_default("t");

        let mut stream = runner
            .run_stream(RunInput {
                prompt: "goal: do something with enough words to exceed one token".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        assert!(compact.call_count() >= 1, "compact provider should be used");
        // 主 provider 仍应参与对话生成，防止整个 runner 误路由到 compact provider
        assert!(
            main.call_count() >= 1,
            "main provider should still be used for sub-agent turns"
        );
    }

    // --- @-mention 调度 ---

    async fn collect_text(runner: &SubAgentRunner, prompt: &str) -> String {
        let mut stream = runner
            .run_stream(RunInput {
                prompt: prompt.to_string(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        let mut out = String::new();
        while let Some(ev) = stream.next().await {
            if let Ok(RunEvent::TextDelta(d)) = ev {
                out.push_str(&d);
            }
        }
        out
    }

    #[tokio::test]
    async fn mention_dispatches_to_named_sub_agent() {
        use crate::test_utils::MockProvider;
        use std::sync::Mutex;

        // 用 per-agent 模型区分 provider（SubAgentRunner 单默认 provider，
        // 模型解析是 per-agent 的 provider 覆盖通道）。
        let coder_p = Arc::new(MockProvider::text("coder answer"));
        let reviewer_p = Arc::new(MockProvider::text("reviewer answer"));
        struct Resolver {
            coder: Arc<MockProvider>,
            reviewer: Arc<MockProvider>,
            called: Mutex<Vec<String>>,
        }
        impl ModelResolver for Resolver {
            fn resolve(&self, name: &str) -> Option<Arc<dyn Provider>> {
                self.called.lock().unwrap().push(name.to_string());
                match name {
                    "m-coder" => Some(self.coder.clone() as Arc<dyn Provider>),
                    "m-reviewer" => Some(self.reviewer.clone() as Arc<dyn Provider>),
                    _ => None,
                }
            }
        }
        let resolver = Arc::new(Resolver {
            coder: coder_p.clone(),
            reviewer: reviewer_p.clone(),
            called: Mutex::new(Vec::new()),
        });
        let mut runner = SubAgentRunner::new(Arc::new(MockProvider::text("default answer")))
            .with_model_resolver(resolver.clone());
        runner.register(
            SubAgentConfig::new("coder", "you are coder").with_model(Some("m-coder".to_string())),
        );
        runner.register(
            SubAgentConfig::new("reviewer", "you are reviewer")
                .with_model(Some("m-reviewer".to_string())),
        );

        let out = collect_text(&runner, "@reviewer please look at the diff").await;
        assert!(out.contains("reviewer answer"), "got: {out}");
        assert_eq!(coder_p.call_count(), 0, "coder must not run");
        assert!(reviewer_p.call_count() >= 1, "reviewer must run");
        let called = resolver.called.lock().unwrap();
        assert_eq!(called.as_slice(), ["m-reviewer"], "got: {called:?}");
    }

    #[tokio::test]
    async fn mention_preserved_in_goal() {
        use crate::test_utils::MockProvider;
        let p = Arc::new(MockProvider::text("ok"));
        let mut runner = SubAgentRunner::new(p.clone());
        runner.register(SubAgentConfig::new("coder", "you are coder"));
        let runner = runner.with_default("coder");

        let _out = collect_text(&runner, "@coder do the thing").await;
        // @引用触发调度后，goal 保留完整 prompt（子代理获知被谁唤起）
        let last = p.last_prompt().unwrap();
        assert!(last.contains("@coder"), "goal should keep mention: {last}");
        assert!(last.contains("do the thing"), "got: {last}");
    }

    #[tokio::test]
    async fn mention_unknown_falls_back_to_default() {
        use crate::test_utils::MockProvider;
        let p = Arc::new(MockProvider::text("default answer"));
        let mut runner = SubAgentRunner::new(p.clone());
        runner.register(SubAgentConfig::new("coder", "you are coder"));
        let runner = runner.with_default("coder");

        let out = collect_text(&runner, "@unknownagent do the thing").await;
        assert!(out.contains("default answer"), "got: {out}");
    }

    #[tokio::test]
    async fn mention_ambiguous_errors() {
        use crate::test_utils::MockProvider;
        let mut runner = SubAgentRunner::new(Arc::new(MockProvider::text("x")));
        runner.register(SubAgentConfig::new("coder", "you are coder"));
        runner.register(SubAgentConfig::new("reviewer", "you are reviewer"));

        let err = runner
            .run_stream(RunInput {
                prompt: "@coder and @reviewer both".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .err()
            .unwrap();
        assert!(err.to_string().contains("ambiguous"), "got: {err}");
    }

    // --- 递归深度上限 ---

    #[tokio::test]
    async fn run_at_depth_beyond_limit_is_rejected() {
        use crate::test_utils::MockProvider;
        let p = Arc::new(MockProvider::text("leaf answer"));
        let mut runner = SubAgentRunner::new(p).with_max_depth(3);
        runner.register(SubAgentConfig::new("leaf", "you are leaf"));

        // depth 3 = 允许；depth 4 > max → 拒绝
        let ok = runner
            .run_at_depth("leaf", "go", &InputValues::new(), 3)
            .await
            .unwrap();
        assert!(ok.contains("leaf answer"), "got: {ok}");

        let err = runner
            .run_at_depth("leaf", "go", &InputValues::new(), 4)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("recursion depth exceeded"),
            "got: {err}"
        );
        assert!(
            err.to_string().contains("3"),
            "max depth should appear: {err}"
        );
    }

    #[tokio::test]
    async fn zero_max_depth_is_clamped_to_one() {
        use crate::test_utils::MockProvider;
        let runner = SubAgentRunner::new(Arc::new(MockProvider::text("x"))).with_max_depth(0);
        // 私有字段在测试内可见
        assert_eq!(runner.max_depth, 1);
    }

    // --- per-agent 模型覆盖 ---

    /// 记录 resolve 请求的模型名，并返回可观测 provider。
    struct RecordingResolver {
        called: std::sync::Mutex<Option<String>>,
        provider: Arc<crate::test_utils::MockProvider>,
    }
    impl ModelResolver for RecordingResolver {
        fn resolve(&self, name: &str) -> Option<Arc<dyn Provider>> {
            *self.called.lock().unwrap() = Some(name.to_string());
            Some(self.provider.clone() as Arc<dyn Provider>)
        }
    }

    #[tokio::test]
    async fn per_agent_model_resolver_is_consulted() {
        use crate::test_utils::MockProvider;
        let resolved = Arc::new(MockProvider::text("resolved-model answer"));
        let resolver = Arc::new(RecordingResolver {
            called: std::sync::Mutex::new(None),
            provider: resolved.clone(),
        });
        let mut runner = SubAgentRunner::new(Arc::new(MockProvider::text("default answer")))
            .with_model_resolver(resolver.clone());
        runner.register(
            SubAgentConfig::new("m", "you are m").with_model(Some("deepseek-v4-flash".to_string())),
        );
        let runner = runner.with_default("m");

        let out = collect_text(&runner, "goal: use your model").await;
        assert!(out.contains("resolved-model answer"), "got: {out}");
        let called = resolver.called.lock().unwrap().clone();
        assert_eq!(called.as_deref(), Some("deepseek-v4-flash"));
        assert!(
            resolved.call_count() >= 1,
            "resolved provider must serve the sub-agent"
        );
    }

    #[tokio::test]
    async fn per_agent_model_without_resolver_falls_back() {
        use crate::test_utils::MockProvider;
        let default_p = Arc::new(MockProvider::text("default answer"));
        let mut runner = SubAgentRunner::new(default_p.clone()); // 无 resolver
        runner.register(
            SubAgentConfig::new("m", "you are m").with_model(Some("deepseek-v4-flash".to_string())),
        );
        let runner = runner.with_default("m");

        let out = collect_text(&runner, "goal: x").await;
        assert!(out.contains("default answer"), "got: {out}");
        assert!(default_p.call_count() >= 1, "default provider must serve");
    }

    // --- per-agent 权限：能力白名单 + gate 模式 ---

    /// 首轮产出指定工具调用，之后把最近一条 Tool 结果回显为最终文本
    /// （便于断言工具结果内容）。
    struct EchoToolProvider {
        tool_name: String,
        args: String,
    }
    #[async_trait::async_trait]
    impl Provider for EchoToolProvider {
        async fn generate(
            &self,
            _v: deepseeknova_provider::ValidatedRequest<'_>,
        ) -> anyhow::Result<Message> {
            Ok(Message {
                role: Role::Assistant,
                content: "echo".into(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            })
        }
        async fn stream(
            &self,
            v: deepseeknova_provider::ValidatedRequest<'_>,
        ) -> anyhow::Result<deepseeknova_core::chunk::ChunkStream> {
            use deepseeknova_core::chunk::{Chunk, Usage};
            let tool_count = v.messages.iter().filter(|m| m.role == Role::Tool).count();
            let chunks: Vec<anyhow::Result<Chunk>> = if tool_count == 0 {
                vec![
                    Ok(Chunk::ToolCallStart {
                        id: "call_1".into(),
                        name: self.tool_name.clone(),
                    }),
                    Ok(Chunk::ToolCallEnd {
                        id: "call_1".into(),
                        name: self.tool_name.clone(),
                        arguments: self.args.clone(),
                    }),
                    Ok(Chunk::Done),
                ]
            } else {
                let last = v
                    .messages
                    .iter()
                    .rev()
                    .find(|m| m.role == Role::Tool)
                    .map(|m| m.content.clone())
                    .unwrap_or_default();
                vec![
                    Ok(Chunk::TextDelta(last)),
                    Ok(Chunk::Usage(Usage::default())),
                    Ok(Chunk::Done),
                ]
            };
            Ok(Box::pin(tokio_stream::iter(chunks)))
        }
    }

    /// 强制 FileWrite 能力的写工具（与真实 fs 工具同构的门禁调用）。
    struct CapWriteTool;
    #[async_trait::async_trait]
    impl Tool for CapWriteTool {
        fn schema(&self) -> deepseeknova_core::ToolSchema {
            deepseeknova_core::ToolSchema {
                name: "write_file".to_string(),
                description: "writes a file".into(),
                parameters: serde_json::json!({}),
            }
        }
        async fn execute(&self, ctx: &ToolContext, _args: &str) -> anyhow::Result<String> {
            deepseeknova_security::context::enforce_capability(
                ctx,
                "write_file",
                deepseeknova_security::capability::Capability::FileWrite,
            )?;
            Ok("written ok".to_string())
        }
    }

    struct NoopReadTool;
    #[async_trait::async_trait]
    impl Tool for NoopReadTool {
        fn schema(&self) -> deepseeknova_core::ToolSchema {
            deepseeknova_core::ToolSchema {
                name: "read_file".to_string(),
                description: "reads".into(),
                parameters: serde_json::json!({}),
            }
        }
        async fn execute(&self, _ctx: &ToolContext, _args: &str) -> anyhow::Result<String> {
            Ok("read ok".to_string())
        }
    }

    #[tokio::test]
    async fn per_agent_capability_whitelist_denies_missing_cap() {
        use crate::agent_manifest::{AgentCapability, AgentGateMode, AgentPermission};
        use deepseeknova_security::context::SecurityContext;

        let p = Arc::new(EchoToolProvider {
            tool_name: "write_file".to_string(),
            args: "{}".into(),
        });
        let mut runner =
            SubAgentRunner::new(p).with_security(SecurityContext::with_safe_defaults());
        // 声明只含 FileRead → FileWrite 被裁掉 → 写工具执行失败
        runner.register(
            SubAgentConfig::new("w", "you write")
                .with_tools(vec![Arc::new(CapWriteTool)])
                .with_permission(AgentPermission {
                    gate: AgentGateMode::Inherit,
                    capabilities: vec![AgentCapability::FileRead],
                }),
        );
        let runner = runner.with_default("w");

        let out = collect_text(&runner, "goal: write a file").await;
        assert!(
            out.contains("Security violation") && out.contains("FileWrite"),
            "写能力被裁剪应失败回显: {out}"
        );
    }

    #[tokio::test]
    async fn per_agent_full_capabilities_allows_tool() {
        use crate::agent_manifest::{AgentCapability, AgentGateMode, AgentPermission};
        use deepseeknova_security::context::SecurityContext;

        let p = Arc::new(EchoToolProvider {
            tool_name: "write_file".to_string(),
            args: "{}".into(),
        });
        let mut runner =
            SubAgentRunner::new(p).with_security(SecurityContext::with_safe_defaults());
        runner.register(
            SubAgentConfig::new("w", "you write")
                .with_tools(vec![Arc::new(CapWriteTool)])
                .with_permission(AgentPermission {
                    gate: AgentGateMode::Inherit,
                    capabilities: vec![AgentCapability::FileWrite],
                }),
        );
        let runner = runner.with_default("w");

        let out = collect_text(&runner, "goal: write a file").await;
        assert!(out.contains("written ok"), "got: {out}");
    }

    #[tokio::test]
    async fn per_agent_fail_closed_without_gate_blocks_all() {
        use crate::agent_manifest::{AgentGateMode, AgentPermission};

        let p = Arc::new(EchoToolProvider {
            tool_name: "read_file".to_string(),
            args: "{}".into(),
        });
        let mut runner = SubAgentRunner::new(p); // 无共享 gate
        runner.register(
            SubAgentConfig::new("f", "you are f")
                .with_tools(vec![Arc::new(NoopReadTool)])
                .with_permission(AgentPermission {
                    gate: AgentGateMode::FailClosed,
                    capabilities: vec![],
                }),
        );
        let runner = runner.with_default("f");

        let out = collect_text(&runner, "goal: read something").await;
        assert!(
            out.contains("fail-closed") && out.contains("blocked"),
            "fail-closed 且无 gate 应拒绝一切工具: {out}"
        );
    }

    #[tokio::test]
    async fn per_agent_gate_none_bypasses_shared_gate() {
        use crate::agent_manifest::{AgentGateMode, AgentPermission};
        use deepseeknova_permission::{Decision, PermissionGate, PolicyBuilder, Rule};

        // 共享 gate 拒绝 read_file
        let gate = Arc::new(PermissionGate::new(
            PolicyBuilder::new()
                .default_mode(Decision::Deny)
                .deny(Rule::new("read_file"))
                .build(),
        ));

        // Inherit 模式 → 被共享 gate 拒绝
        let p_inherit = Arc::new(EchoToolProvider {
            tool_name: "read_file".to_string(),
            args: "{}".into(),
        });
        let mut inherit_runner = SubAgentRunner::new(p_inherit).with_permission_gate(gate.clone());
        inherit_runner.register(
            SubAgentConfig::new("f", "you are f")
                .with_tools(vec![Arc::new(NoopReadTool)])
                .with_permission(AgentPermission {
                    gate: AgentGateMode::Inherit,
                    capabilities: vec![],
                }),
        );
        let inherit_runner = inherit_runner.with_default("f");
        let out_inherit = collect_text(&inherit_runner, "goal: read").await;
        assert!(
            out_inherit.contains("blocked by permission policy"),
            "Inherit 应被共享 gate 拒绝: {out_inherit}"
        );

        // None 模式 → 绕过共享 gate，工具直接执行
        let p_none = Arc::new(EchoToolProvider {
            tool_name: "read_file".to_string(),
            args: "{}".into(),
        });
        let mut none_runner = SubAgentRunner::new(p_none).with_permission_gate(gate);
        none_runner.register(
            SubAgentConfig::new("f", "you are f")
                .with_tools(vec![Arc::new(NoopReadTool)])
                .with_permission(AgentPermission {
                    gate: AgentGateMode::None,
                    capabilities: vec![],
                }),
        );
        let none_runner = none_runner.with_default("f");
        let out_none = collect_text(&none_runner, "goal: read").await;
        assert!(out_none.contains("read ok"), "None 应绕过 gate: {out_none}");
    }
}
