use crate::agent::fire_user_notify_hooks;
use crate::agent_manifest::{AgentGateMode, AgentPermission};
use crate::memory::Memory;
use crate::mention::resolve_mention;
use crate::recursion::{DelegateDepth, DelegationSink};
use crate::task_spec::{InputValues, TaskSpec};
use deepseeknova_core::chunk::{Chunk, Usage};
use deepseeknova_core::tool_hook::{
    run_user_hook, HookEvent, HookPayload, HookVerdict, ToolHook, ToolHookCtx, UserHooks,
};
use deepseeknova_core::types::{FunctionCall, ToolCall};
use deepseeknova_core::{
    DeepseeknovaError, Message, Role, RunEvent, RunEventStream, RunInput, RunOutput, Runner, Tool,
    ToolContext,
};
use deepseeknova_provider::Provider;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Approximate characters-per-token for rough heuristics.
use crate::tokens::estimate_tokens;

/// Mutex 毒化恢复：另一线程在持锁临界区内 panic 后，`lock()` 返回
/// `PoisonError`。`into_inner()` 取回被污染的数据（可能不完整），warn 记录
/// 后再继续——比 `unwrap()` 崩溃更优雅，语义与项目其他 poison 处理对齐。
fn recover_poisoned<T>(e: std::sync::PoisonError<T>) -> T {
    warn!("mutex poisoned by a panicking thread; recovering locked value");
    e.into_inner()
}

/// 子代理递归深度上限默认值（根派发 depth 1；可再派 depth 2…直至上限）。
/// D2：3→5（对齐 Claude Code 嵌套 5 层；`DelegateDepth` 有界递归防死循环）。
pub const DEFAULT_MAX_DEPTH: usize = 5;

/// per-agent 模型解析：把声明模型名解析为 provider 实例。由上层
/// （runtime / CLI）基于 provider 工厂实现；未装配时声明模型回退默认 provider。
pub trait ModelResolver: Send + Sync {
    /// 把声明模型名解析为 provider；返回 `None` 表示回退默认 provider。
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
    /// 子代理名（委派目标标识）。
    pub name: String,
    /// 角色系统提示。
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
    /// 以名字 + 系统提示构造配置（其余参数取默认：无工具、10 步、默认权限）。
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

    /// 设置执行用工具集（同时同步 spec.tools 名字白名单）。
    pub fn with_tools(mut self, tools: Vec<Arc<dyn Tool>>) -> Self {
        self.spec.tools = tools.iter().map(|t| t.schema().name.clone()).collect();
        self.tools = tools;
        self
    }

    /// 设置执行步数上限（`0` 视作默认 10；同步 spec.max_steps 描述）。
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
///
/// `Clone` 为运行时递归接线提供：`runner.clone()` 与 `delegation_sink` 槽共享
/// 同一 `Arc<Mutex>`，故 `set_delegation_sink(Arc::new(runner.clone()))` 后
/// 原 runner 与克隆体可互相递归派发。
#[derive(Clone)]
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
    /// 工具生命周期钩子链（任务质量闭环）：before/after 按注册顺序串行执行。
    /// 与主 agent 路径对称——子代理工具调用同样经过 QualityHook 的
    /// 禁写路径/secret 检测/写后策略评估。panic 契约：before panic →
    /// Deny（fail-closed），after panic → 空 findings（fail-open）。
    tool_hooks: Vec<Arc<dyn ToolHook>>,
    /// 用户级外部 hooks（`[hooks]` 配置）。tool_before 为 AND 链额外一层
    /// （fail-closed）；tool_after 失败仅 warn。空 `UserHooks` = 零进程开销。
    user_hooks: UserHooks,
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
    /// 以默认 provider 构造 runner（其余设置经 builder 装配）。
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self {
            provider,
            compact_provider: None,
            sub_agents: HashMap::new(),
            default_sub_agent: None,
            compaction_threshold_tokens: None,
            permission: None,
            tool_hooks: Vec::new(),
            user_hooks: UserHooks::default(),
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

    /// 已注册子代理名（排序稳定），供上层 @-mention 入口做已知名预检。
    pub fn agent_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.sub_agents.keys().cloned().collect();
        v.sort();
        v
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

    /// 注入工具生命周期钩子（任务质量闭环）。可多次调用注册多个钩子，
    /// before/after 按注册顺序串行执行。子代理工具调用与主 agent 路径
    /// 对称——同样经过 QualityHook 的禁写路径/secret 检测/写后策略评估。
    /// panic 契约：before panic → Deny（fail-closed），after panic →
    /// 空 findings（fail-open，不阻断执行）。
    pub fn with_tool_hook(mut self, hook: Arc<dyn ToolHook>) -> Self {
        self.tool_hooks.push(hook);
        self
    }

    /// 挂载用户级外部 hooks（`[hooks]` 配置）。`tool_before` 在内部
    /// tool_hook 链之外额外一层（AND 链：内部链 + 用户 hooks 都过才执行；
    /// 任一命令非 0 退出 / 超时 / 崩溃 → 阻止执行，fail-closed）；
    /// `tool_after` 失败仅 warn。空 `UserHooks` = 零进程开销。
    pub fn with_user_hooks(mut self, hooks: UserHooks) -> Self {
        self.user_hooks = hooks;
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
        *self.delegation_sink.lock().unwrap_or_else(recover_poisoned) = Some(sink);
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
    fn resolve_sub_agent(
        &self,
        name: Option<String>,
    ) -> Result<&SubAgentConfig, DeepseeknovaError> {
        if let Some(ref n) = name {
            self.sub_agents
                .get(n)
                .ok_or_else(|| DeepseeknovaError::agent(format!("unknown sub-agent: '{n}'")))
        } else if let Some(ref default) = self.default_sub_agent {
            self.sub_agents.get(default).ok_or_else(|| {
                DeepseeknovaError::agent(format!("default sub-agent '{default}' not registered"))
            })
        } else {
            Err(DeepseeknovaError::agent(
                "no sub-agent specified and no default configured. \
                 Use 'sub_agent:<name>' in the prompt or register a default.",
            ))
        }
    }

    /// @-mention 解析：返回首个**已知**子代理引用（零个 → None）。
    /// 多个已知引用 → Err（消歧失败，提示用户一次引用一个）。
    fn resolve_mention(&self, prompt: &str) -> Result<Option<String>, DeepseeknovaError> {
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
    ) -> Result<String, DeepseeknovaError> {
        self.run_at_depth_with_parent_cancel(agent, goal, values, depth, None)
            .await
    }

    /// 带深度派发 + 父取消传播：行为与 [`Self::run_at_depth`] 完全一致，额外
    /// 把父 run 的 [`CancellationToken`] 传进子代理循环（`dispatch_stream` →
    /// `run_sub_agent_loop`）。父取消后，子代理在步边界检查或工具执行中途
    /// （`tokio::select!` 抢占）立即中止，不再跑满 `max_steps`；`None` =
    /// 顶层派发（无父 run），子代理使用独立取消令牌。
    pub async fn run_at_depth_with_parent_cancel(
        &self,
        agent: &str,
        goal: &str,
        values: &InputValues,
        depth: usize,
        parent_cancel: Option<CancellationToken>,
    ) -> Result<String, DeepseeknovaError> {
        let mut stream = self
            .dispatch_stream(
                Some(agent.to_string()),
                goal.to_string(),
                values.clone(),
                depth,
                parent_cancel,
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
            return Err(DeepseeknovaError::runner(format!(
                "sub-agent '{agent}' produced no output"
            )));
        }
        Ok(text)
    }

    /// 核心派发：解析配置 → 渲染任务书 → 应用 per-agent 模型/权限 → spawn 循环。
    /// `parent_cancel`：父 run 的取消令牌（`None` = 顶层派发）；透传给
    /// [`run_sub_agent_loop`]，父取消即中止子代理。
    async fn dispatch_stream(
        &self,
        sub_agent_name: Option<String>,
        goal: String,
        parsed_inputs: InputValues,
        depth: usize,
        parent_cancel: Option<CancellationToken>,
    ) -> Result<RunEventStream, DeepseeknovaError> {
        // 深度上限：超深拒绝（递归守门；根派发 depth 1）。
        if depth > self.max_depth {
            return Err(DeepseeknovaError::runner(format!(
                "sub-agent recursion depth exceeded (max {}, depth requested: {depth})",
                self.max_depth
            )));
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
        // - None → 绕过 gate（工具直接执行）；记录安全审计 warn，建议改用
        //   Inherit 或 FailClosed。None 模式下子代理无审批通道兜底，
        //   仅靠 QualityHook 链（若已注入）做最低安全门。
        // - FailClosed → 共享 gate；无共享 gate 时在循环内拒绝一切工具
        let (permission, fail_closed) = match config.permission.gate {
            AgentGateMode::Inherit => (self.permission.clone(), false),
            AgentGateMode::None => {
                warn!(
                    security_event = "subagent_gate_none",
                    sub_agent = %config.name,
                    "sub-agent configured with gate=None: permission gate bypassed; \
                     QualityHook chain (if any) is the sole safety barrier"
                );
                (None, false)
            }
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
        let tool_hooks = self.tool_hooks.clone();
        let user_hooks = self.user_hooks.clone();
        let delegation_sink = self
            .delegation_sink
            .lock()
            .unwrap_or_else(recover_poisoned)
            .clone();
        let mut system_prompt = crate::prompts::compose_sub_agent_prompt(&config.system_prompt);
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
            reasoning_signature: None,
        });

        info!(
            sub_agent = %config.name,
            goal = %goal,
            max_steps = max_steps,
            depth = depth,
            "dispatching sub-agent"
        );

        // 捕获子代理名（config 在 move 后不可用；loop 内日志需要）
        let sub_agent_name = config.name.clone();

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
                tool_hooks,
                user_hooks,
                sub_agent_name,
                parent_cancel,
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
    ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
        self.run_at_depth(agent, goal, values, depth).await
    }
}

#[async_trait::async_trait]
impl Runner for SubAgentRunner {
    async fn run_stream(
        &self,
        input: RunInput,
    ) -> Result<RunEventStream, deepseeknova_core::DeepseeknovaError> {
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

        self.dispatch_stream(sub_agent_name, goal, parsed_inputs, 1, None)
            .await
    }
}

/// 子代理事件流包装：`tokio::spawn` 的子代理任务不随调用方 future 被丢弃而
/// 取消（timeout/提前返回会 drop 掉 `run_stream` 的 future，但 spawn 的任务
/// 会继续跑到 `max_steps`，导致重试并发 fan-out）。本包装在 stream 被 drop
/// 时 abort 子代理任务，阻断后台执行与重复副作用（Bugbot 审查 HIGH-2 修复）。
struct AbortOnDropStream {
    inner: mpsc::Receiver<Result<RunEvent, DeepseeknovaError>>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl tokio_stream::Stream for AbortOnDropStream {
    type Item = Result<RunEvent, DeepseeknovaError>;

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
    tx: &mpsc::Sender<Result<RunEvent, DeepseeknovaError>>,
    permission: Option<Arc<deepseeknova_permission::PermissionGate>>,
    security: Option<deepseeknova_security::context::SecurityContext>,
    workspace_root: std::path::PathBuf,
    depth: usize,
    delegation_sink: Option<Arc<dyn DelegationSink>>,
    fail_closed: bool,
    tool_hooks: Vec<Arc<dyn ToolHook>>,
    user_hooks: UserHooks,
    sub_agent_name: String,
    parent_cancel: Option<CancellationToken>,
) -> Result<(), DeepseeknovaError> {
    // T12：父 run 取消传播——有父令牌时取其子令牌（父取消级联到本循环），
    // 无父（顶层派发）用独立令牌。工具执行包 `tokio::select!`，父取消立即中断。
    let cancel = match &parent_cancel {
        Some(parent) => parent.child_token(),
        None => CancellationToken::new(),
    };

    // 构建工具执行上下文：工作区 + 安全上下文 + 递归深度 + 递归派发出口。
    // 递归深度扩展供 `RecursiveDelegateTool` 读取当前深度（再派时 +1）。
    // 取消令牌由本循环统一持有（父派生/独立），注入每个工具执行 ctx。
    let build_ctx = |call_id: &str| {
        let mut ctx = ToolContext::with_cancellation(call_id.to_string(), cancel.child_token())
            .with_workspace(workspace_root.clone());
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
        reasoning_signature: None,
    });

    // T12：资源限额步级检查（对齐主循环 loop_impl.rs 的 step 边界判定）。
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

        // T12：步级限额——整体墙钟截止 + 工具调用预算（与主循环同口径）。
        if let Some(sec) = &security {
            if run_started.elapsed() > sec.limits.max_execution_time {
                warn!(
                    "sub-agent exceeded max_execution_time ({:?})",
                    sec.limits.max_execution_time
                );
                return Err(DeepseeknovaError::runner(format!(
                    "sub-agent exceeded max execution time ({:?})",
                    sec.limits.max_execution_time
                )));
            }
            if tool_calls_made >= sec.limits.max_tool_calls {
                warn!(
                    "sub-agent exceeded max_tool_calls ({})",
                    sec.limits.max_tool_calls
                );
                return Err(DeepseeknovaError::runner(format!(
                    "sub-agent exceeded max tool calls ({})",
                    sec.limits.max_tool_calls
                )));
            }
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
                DeepseeknovaError::runner(format!(
                    "history replay invariant violated in sub-agent: {} violation(s)",
                    violations.len()
                ))
            })?;

        // Stream from provider
        let mut stream = provider.stream(validated).await?;
        let mut text_buf = String::new();
        let mut reasoning_buf = String::new();
        // T12 收尾：流式 signature 随 reasoning 保存（多轮回放必需）。
        let mut reasoning_signature: Option<String> = None;
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
                    if reasoning_signature.is_none() {
                        reasoning_signature = signature.clone();
                    }
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
                reasoning_signature: reasoning_signature.clone(),
            });

            // 2) 逐个执行（gate 检查 → ToolHook before → 执行 → ToolHook after），
            //    回填 Tool 消息。钩子链与主 agent 路径对称：
            //    - before Deny → 回填阻断结果（不执行）
            //    - before Ask → 子代理无审批通道，fail-closed 拒绝
            //    - after → findings 仅记录（fail-open，不阻断）
            for (id, name, arguments) in &tool_calls {
                let result: String = match tools.iter().find(|t| &t.schema().name == name) {
                    None => format!("Error: unknown tool '{name}'"),
                    Some(tool) => {
                        // ── 阶段 1：权限门检查 ──
                        let gate_block: Option<String> = {
                            let verdict = permission
                                .as_ref()
                                .map(|g| g.check(tool.as_ref(), arguments));
                            match verdict {
                                None if !fail_closed => None,
                                None => Some(format!(
                                    "Error: tool '{name}' blocked by permission policy: \
                                     sub-agent is fail-closed and no permission gate is configured"
                                )),
                                Some(v) => match v.decision() {
                                    deepseeknova_permission::Decision::Allow => None,
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
                                        Some(msg)
                                    }
                                    deepseeknova_permission::Decision::Ask => Some(format!(
                                        "Error: tool '{name}' requires approval \
                                         (sub-agent has no approval channel; treat as denied)"
                                    )),
                                },
                            }
                        };

                        // ── 阶段 2：ToolHook before 链（gate 通过后串行执行）──
                        // 决策合并：任一 Deny → 拒绝；任一 Ask → fail-closed 拒绝
                        // （子代理无审批通道）；全 Allow → 放行。
                        // panic 契约：before panic → Deny（fail-closed）。
                        let hook_block = if gate_block.is_none() && !tool_hooks.is_empty() {
                            let hook_call = ToolCall {
                                id: id.clone(),
                                ty: "function".to_string(),
                                function: FunctionCall {
                                    name: name.clone(),
                                    arguments: arguments.clone(),
                                },
                            };
                            let ctx = ToolHookCtx {
                                workspace_root: &workspace_root,
                            };
                            let mut denied: Option<String> = None;
                            for hook in &tool_hooks {
                                let interested =
                                    catch_unwind(AssertUnwindSafe(|| hook.interested(&hook_call)))
                                        .unwrap_or_else(|_| {
                                            warn!(
                                                "tool hook '{}' panicked in interested(); \
                                         treated as not interested",
                                                hook.name()
                                            );
                                            false
                                        });
                                if !interested {
                                    continue;
                                }
                                let verdict = catch_unwind(AssertUnwindSafe(|| {
                                    hook.before(&ctx, &hook_call)
                                }))
                                .unwrap_or_else(|_| {
                                    warn!(
                                        "tool hook '{}' panicked in before() \
                                                 → deny (fail-closed)",
                                        hook.name()
                                    );
                                    HookVerdict::Deny(format!(
                                        "tool hook '{}' panicked during pre-check \
                                                 (fail-closed deny)",
                                        hook.name()
                                    ))
                                });
                                match verdict {
                                    HookVerdict::Allow | HookVerdict::AllowWith(_) => {}
                                    HookVerdict::Deny(reason) => {
                                        denied = Some(reason);
                                        break;
                                    }
                                    HookVerdict::Ask(reason) => {
                                        // 子代理无审批通道，Ask 等同 Deny（fail-closed）。
                                        denied = Some(format!(
                                            "tool hook '{}' requested approval \
                                             (sub-agent has no approval channel): {}",
                                            hook.name(),
                                            reason
                                        ));
                                        break;
                                    }
                                }
                            }
                            denied
                        } else {
                            None
                        };

                        // ── 阶段 2b：用户级外部 hooks tool_before（AND 链，fail-closed）──
                        // 内部 tool_hook 链 + 用户 hooks 都过才执行。任一命令非 0
                        // 退出 / 超时 / 崩溃，或 stdout 裁决 `allowed=false` → 阻止。
                        // 仅在 gate 与内部 hook 均未阻断时运行（避免冗余进程）。
                        let user_block = if gate_block.is_none()
                            && hook_block.is_none()
                            && !user_hooks.tool_before.is_empty()
                        {
                            let payload = HookPayload {
                                event: HookEvent::ToolBefore.as_str(),
                                tool: Some(name),
                                arguments: Some(arguments),
                                workspace: &workspace_root,
                                session_id: &sub_agent_name,
                            };
                            let mut denied: Option<String> = None;
                            for cmd in &user_hooks.tool_before {
                                let run = run_user_hook(cmd, &payload).await;
                                if !run.exec.is_allowed() {
                                    denied = Some(format!(
                                        "blocked by user hook '{}' (fail-closed: {:?})",
                                        cmd.command, run.exec
                                    ));
                                    break;
                                }
                                if let Some(v) = run.verdict {
                                    if !v.allowed {
                                        denied = Some(if v.reason.is_empty() {
                                            format!("denied by user hook '{}'", cmd.command)
                                        } else {
                                            v.reason.clone()
                                        });
                                        break;
                                    }
                                }
                            }
                            denied
                        } else {
                            None
                        };

                        // gate 或 hook 阻断 → 回填错误，不执行工具
                        if let Some(reason) = gate_block.or(hook_block).or(user_block) {
                            reason
                        } else {
                            // ── 阶段 3：执行工具（父取消即中断）──
                            // T12：工具执行包 `tokio::select!`——父 run 取消
                            // （级联到本循环 `cancel`）时立即丢弃工具 future，
                            // 不再跑满 max_steps。
                            let ctx = build_ctx(id);
                            // T12：递归委派工具能力门（对齐 DelegateTool 的
                            // enforce_capability）。RecursiveDelegateTool 本体在
                            // recursion.rs（本次改动范围外），子代理执行边界在此
                            // 补同款门禁：委派隐含命令执行，需 CommandExecute
                            // 能力。SecurityContext 存在时未授予 → fail-closed
                            // 阻断（不执行工具）；未装配 SecurityContext 的子
                            // 代理（测试/库级裸装配）保持既有行为不拦截。
                            let cap_block: Option<String> = if name == "delegate"
                                && ctx
                                    .extensions
                                    .get::<deepseeknova_security::context::SecurityContext>()
                                    .is_some()
                            {
                                deepseeknova_security::context::enforce_capability(
                                    &ctx,
                                    "delegate",
                                    deepseeknova_security::capability::Capability::CommandExecute,
                                )
                                .err()
                                .map(|e| format!("Error: tool '{name}' {e}"))
                            } else {
                                None
                            };
                            let output = if let Some(blocked) = cap_block {
                                blocked
                            } else {
                                tool_calls_made += 1;
                                let exec_result = tokio::select! {
                                    r = tool.execute(&ctx, arguments) => r,
                                    _ = cancel.cancelled() => Err(
                                        deepseeknova_core::DeepseeknovaError::Cancelled
                                    ),
                                };
                                match exec_result {
                                    Ok(out) => out,
                                    Err(e) => format!("Error: {e}"),
                                }
                            };
                            // T12：工具输出统一 `max_output_bytes` 截断（含非
                            // shell 工具；方式与 shell.rs cap_output / 主循环
                            // execute_tool_call 对齐——UTF-8 字符边界 + 截断标记）。
                            // SecurityContext 缺失时不截断（无配额可依）。
                            let output = match security.as_ref() {
                                Some(sec)
                                    if output.len() > sec.limits.max_output_bytes as usize =>
                                {
                                    let max_out = sec.limits.max_output_bytes as usize;
                                    let end = output.floor_char_boundary(max_out);
                                    format!(
                                        "{}... [truncated {} bytes]",
                                        &output[..end],
                                        output.len() - end
                                    )
                                }
                                _ => output,
                            };

                            // ── 阶段 4：ToolHook after 写后评估（fail-open）──
                            // findings 仅记录到日志（子代理路径不产事件流）；
                            // after panic → 空 findings（不阻断执行）。
                            if !tool_hooks.is_empty() && !output.starts_with("Error:") {
                                let hook_call = ToolCall {
                                    id: id.clone(),
                                    ty: "function".to_string(),
                                    function: FunctionCall {
                                        name: name.clone(),
                                        arguments: arguments.clone(),
                                    },
                                };
                                let ctx = ToolHookCtx {
                                    workspace_root: &workspace_root,
                                };
                                for hook in &tool_hooks {
                                    let interested = catch_unwind(AssertUnwindSafe(|| {
                                        hook.interested(&hook_call)
                                    }))
                                    .unwrap_or_else(|_| {
                                        warn!(
                                            "tool hook '{}' panicked in interested(); \
                                             treated as not interested",
                                            hook.name()
                                        );
                                        false
                                    });
                                    if !interested {
                                        continue;
                                    }
                                    let findings = catch_unwind(AssertUnwindSafe(|| {
                                        hook.after(&ctx, &hook_call, &output)
                                    }))
                                    .unwrap_or_else(|_| {
                                        warn!(
                                            "tool hook '{}' panicked in after(); \
                                             fail-open empty findings",
                                            hook.name()
                                        );
                                        Vec::new()
                                    });
                                    for finding in &findings {
                                        if finding.severity
                                            == deepseeknova_core::tool_hook::FindingSeverity::Blocking
                                        {
                                            warn!(
                                                security_event = "subagent_quality_blocking",
                                                sub_agent = %sub_agent_name,
                                                tool = %name,
                                                rule = %finding.rule,
                                                evidence = %finding.evidence,
                                                "blocking quality finding from sub-agent tool hook"
                                            );
                                        }
                                    }
                                }
                            }

                            // ── 阶段 4b：用户级外部 hooks tool_after（通知型，fail-open）──
                            // 失败仅 warn，不阻断。空列表零开销。
                            if !user_hooks.tool_after.is_empty() && !output.starts_with("Error:") {
                                let payload = HookPayload {
                                    event: HookEvent::ToolAfter.as_str(),
                                    tool: Some(name),
                                    arguments: Some(arguments),
                                    workspace: &workspace_root,
                                    session_id: &sub_agent_name,
                                };
                                fire_user_notify_hooks(&user_hooks.tool_after, &payload);
                            }
                            output
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
                    reasoning_signature: None,
                });
            }
            // T12：工具执行被父取消中断 → 立即中止（不再续步跑满 max_steps）。
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
                reasoning_signature: reasoning_signature.clone(),
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
            reasoning_signature: reasoning_signature.clone(),
        });
    }

    warn!("sub-agent reached max steps ({max_steps})");
    Err(DeepseeknovaError::runner(format!(
        "sub-agent reached max steps ({max_steps}) without completing the task"
    )))
}

/// Build a compaction digest by asking the provider to summarize old messages.
async fn compact_with_provider(
    provider: &dyn Provider,
    messages: &[Message],
) -> Result<String, DeepseeknovaError> {
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
        reasoning_signature: None,
    }];

    let validated =
        deepseeknova_provider::ValidatedRequest::new(&summary_msgs, &[]).map_err(|v| {
            DeepseeknovaError::runner(format!(
                "invariant violation in sub-agent summarize: {:?}",
                v
            ))
        })?;
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
            async fn execute(
                &self,
                _ctx: &ToolContext,
                _args: &str,
            ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
                Ok("done".to_string())
            }
        }

        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(DummyTool)];
        let config = SubAgentConfig::new("test", "prompt").with_tools(tools);
        assert_eq!(config.tools.len(), 1);
        assert_eq!(config.tools[0].schema().name, "dummy");
    }

    #[tokio::test]
    async fn system_prompt_orders_baseline_role_denies_and_rendered_rules() {
        use deepseeknova_core::chunk::Chunk;
        use std::sync::Mutex;

        struct SystemCaptureProvider {
            seen: Arc<Mutex<Vec<Message>>>,
        }

        #[async_trait::async_trait]
        impl Provider for SystemCaptureProvider {
            async fn generate(
                &self,
                _validated: deepseeknova_provider::ValidatedRequest<'_>,
            ) -> Result<Message, DeepseeknovaError> {
                Ok(Message {
                    role: Role::Assistant,
                    content: "done".into(),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                    reasoning_signature: None,
                })
            }

            async fn stream(
                &self,
                validated: deepseeknova_provider::ValidatedRequest<'_>,
            ) -> Result<deepseeknova_core::chunk::ChunkStream, DeepseeknovaError> {
                *self.seen.lock().unwrap() = validated.messages.to_vec();
                Ok(Box::pin(tokio_stream::iter(vec![
                    Ok(Chunk::TextDelta("done".into())),
                    Ok(Chunk::Done),
                ])))
            }
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        let spec = TaskSpec {
            name: "reviewer".into(),
            task: String::new(),
            rules: vec!["RENDERED_RULE_MARKER".into()],
            inputs: Vec::new(),
            tools: Vec::new(),
            max_steps: 2,
        };
        let mut runner =
            SubAgentRunner::new(Arc::new(SystemCaptureProvider { seen: seen.clone() }));
        runner.register(
            SubAgentConfig::new("reviewer", "ROLE_PROMPT_MARKER")
                .with_task_spec(spec)
                .with_frozen_denies(vec!["DENY_MARKER".into()]),
        );
        let runner = runner.with_default("reviewer");

        let mut stream = runner
            .run_stream(RunInput {
                prompt: "review the change".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        let system = seen
            .lock()
            .unwrap()
            .iter()
            .find(|message| message.role == Role::System)
            .expect("sub-agent must receive a system prompt")
            .content
            .clone();
        let baseline = system
            .find("# DeepseekNova Agent — Execution Contract")
            .expect("shared baseline missing");
        let role = system
            .find("ROLE_PROMPT_MARKER")
            .expect("role prompt missing");
        let deny = system.find("DENY_MARKER").expect("frozen deny missing");
        let rules = system
            .find("RENDERED_RULE_MARKER")
            .expect("rendered rules missing");
        assert!(baseline < role && role < deny && deny < rules, "{system}");
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
        ) -> Result<Message, DeepseeknovaError> {
            Ok(Message {
                role: Role::Assistant,
                content: "mock".to_string(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                reasoning_signature: None,
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
            reasoning_signature: None,
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

    // --- Mutex 毒化恢复（L3）---

    #[test]
    fn delegation_sink_poison_recovers_without_panic() {
        use crate::test_utils::MockProvider;
        let runner = SubAgentRunner::new(Arc::new(MockProvider::text("x")));

        // 毒化 delegation_sink：另一线程持锁 panic，释放后 mutex 进入 poisoned 态。
        let m = Arc::clone(&runner.delegation_sink);
        let t = std::thread::spawn(move || {
            let _guard = m.lock().unwrap();
            panic!("intentional poison");
        });
        assert!(t.join().is_err(), "poisoning thread must panic");
        assert!(runner.delegation_sink.is_poisoned());

        // 写路径（set_delegation_sink）不得 panic，须恢复并写入。
        let sink: Arc<dyn DelegationSink> = Arc::new(runner.clone());
        runner.set_delegation_sink(sink);

        // 读路径（与 run_stream 克隆 delegation_sink 槽同款）经 recover_poisoned 恢复。
        let stored = runner
            .delegation_sink
            .lock()
            .unwrap_or_else(recover_poisoned)
            .clone();
        assert!(
            stored.is_some(),
            "sink must be stored after poison recovery"
        );
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
        ) -> Result<Message, DeepseeknovaError> {
            Ok(Message {
                role: Role::Assistant,
                content: "echo".into(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                reasoning_signature: None,
            })
        }
        async fn stream(
            &self,
            v: deepseeknova_provider::ValidatedRequest<'_>,
        ) -> Result<deepseeknova_core::chunk::ChunkStream, DeepseeknovaError> {
            use deepseeknova_core::chunk::{Chunk, Usage};
            let tool_count = v.messages.iter().filter(|m| m.role == Role::Tool).count();
            let chunks: Vec<Result<Chunk, DeepseeknovaError>> = if tool_count == 0 {
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
        async fn execute(
            &self,
            ctx: &ToolContext,
            _args: &str,
        ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
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
        async fn execute(
            &self,
            _ctx: &ToolContext,
            _args: &str,
        ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
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

    // -----------------------------------------------------------------------
    // T12：父取消传播 / 步级限额 / 输出截断 / 递归委派能力门
    // -----------------------------------------------------------------------

    /// 直接驱动 `run_sub_agent_loop` 的最小装配。
    async fn drive_sub_agent_loop(
        provider: Arc<dyn Provider>,
        tools: Vec<Arc<dyn Tool>>,
        security: Option<deepseeknova_security::context::SecurityContext>,
        parent_cancel: Option<CancellationToken>,
        memory: &mut Memory,
    ) -> Result<(), DeepseeknovaError> {
        let (tx, _rx) = mpsc::channel(64);
        run_sub_agent_loop(
            provider.clone(),
            provider,
            tools,
            100,
            None,
            memory,
            "goal: test".to_string(),
            &tx,
            None,
            security,
            std::env::temp_dir(),
            1,
            None,
            false,
            Vec::new(),
            UserHooks::default(),
            "sub-test".to_string(),
            parent_cancel,
        )
        .await
    }

    /// T12：父取消传播——工具执行中被父取消立即中断，不再跑满 max_steps。
    #[tokio::test]
    async fn parent_cancel_aborts_sub_agent_during_tool_execution() {
        use crate::test_utils::MockProvider;

        // 永不完成的工具：依赖 `select!` 的父取消分支打断。
        struct BlockingTool;
        #[async_trait::async_trait]
        impl Tool for BlockingTool {
            fn schema(&self) -> deepseeknova_core::ToolSchema {
                deepseeknova_core::ToolSchema {
                    name: "block".to_string(),
                    description: "blocks forever".to_string(),
                    parameters: serde_json::json!({}),
                }
            }
            async fn execute(
                &self,
                _ctx: &ToolContext,
                _args: &str,
            ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
                std::future::pending::<()>().await;
                Ok(String::new())
            }
        }
        let provider = Arc::new(MockProvider::sequential(vec![
            vec![
                Chunk::ToolCallStart {
                    id: "b1".into(),
                    name: "block".into(),
                },
                Chunk::ToolCallEnd {
                    id: "b1".into(),
                    name: "block".into(),
                    arguments: "{}".into(),
                },
                Chunk::Done,
            ],
            vec![
                Chunk::TextDelta("done".into()),
                Chunk::Usage(Usage::default()),
                Chunk::Done,
            ],
        ]));

        let parent = CancellationToken::new();
        let parent_for_task = parent.clone();
        let sec = deepseeknova_security::context::SecurityContext::with_safe_defaults();
        let handle = tokio::spawn(async move {
            let mut memory = Memory::new();
            drive_sub_agent_loop(
                provider,
                vec![Arc::new(BlockingTool)],
                Some(sec),
                Some(parent_for_task),
                &mut memory,
            )
            .await
        });
        // 让子代理进入工具执行（阻塞在 pending future）。
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        parent.cancel();
        // 父取消后子代理应立即返回（select! 抢占 + 步边界检查）。
        let joined = tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("sub-agent must abort promptly after parent cancel")
            .expect("loop future must not panic");
        assert!(joined.is_ok(), "cancel path returns Ok, got: {joined:?}");
    }

    /// T12：工具输出统一 `max_output_bytes` 截断（含非 shell 工具）。
    #[tokio::test]
    async fn sub_agent_truncates_tool_output_at_max_output_bytes() {
        use crate::test_utils::MockProvider;

        struct LongTool;
        #[async_trait::async_trait]
        impl Tool for LongTool {
            fn schema(&self) -> deepseeknova_core::ToolSchema {
                deepseeknova_core::ToolSchema {
                    name: "long".to_string(),
                    description: "long output tool".to_string(),
                    parameters: serde_json::json!({}),
                }
            }
            async fn execute(
                &self,
                _ctx: &ToolContext,
                _args: &str,
            ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
                Ok("a".repeat(500))
            }
        }
        let provider = Arc::new(MockProvider::sequential(vec![
            vec![
                Chunk::ToolCallStart {
                    id: "l1".into(),
                    name: "long".into(),
                },
                Chunk::ToolCallEnd {
                    id: "l1".into(),
                    name: "long".into(),
                    arguments: "{}".into(),
                },
                Chunk::Done,
            ],
            vec![
                Chunk::TextDelta("done".into()),
                Chunk::Usage(Usage::default()),
                Chunk::Done,
            ],
        ]));
        let mut sec = deepseeknova_security::context::SecurityContext::with_safe_defaults();
        sec.limits.max_output_bytes = 32;

        let mut memory = Memory::new();
        let result = drive_sub_agent_loop(
            provider,
            vec![Arc::new(LongTool)],
            Some(sec),
            None,
            &mut memory,
        )
        .await;
        assert!(result.is_ok(), "loop must complete: {result:?}");
        let msgs = memory.get_all();
        let tool_msg = msgs
            .iter()
            .find(|m| m.role == Role::Tool)
            .expect("tool result must be stored in memory");
        assert!(
            tool_msg.content.contains("[truncated"),
            "oversized output must be truncated: {}",
            tool_msg.content
        );
        assert!(
            tool_msg.content.len() < 500,
            "truncated output must be much shorter than original"
        );
    }

    /// T12：步级限额——`max_tool_calls` 超限后步边界中止（对齐主循环）。
    #[tokio::test]
    async fn sub_agent_stops_at_max_tool_calls() {
        use crate::test_utils::MockProvider;

        struct OkTool;
        #[async_trait::async_trait]
        impl Tool for OkTool {
            fn schema(&self) -> deepseeknova_core::ToolSchema {
                deepseeknova_core::ToolSchema {
                    name: "t".to_string(),
                    description: "ok tool".to_string(),
                    parameters: serde_json::json!({}),
                }
            }
            async fn execute(
                &self,
                _ctx: &ToolContext,
                _args: &str,
            ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
                Ok("ok".to_string())
            }
        }
        // 单响应重放：每步都产工具调用 → 循环执行工具直到限额触发。
        let provider = Arc::new(MockProvider::new(vec![
            Chunk::ToolCallStart {
                id: "t1".into(),
                name: "t".into(),
            },
            Chunk::ToolCallEnd {
                id: "t1".into(),
                name: "t".into(),
                arguments: "{}".into(),
            },
            Chunk::Done,
        ]));
        let mut sec = deepseeknova_security::context::SecurityContext::with_safe_defaults();
        sec.limits.max_tool_calls = 1;

        let mut memory = Memory::new();
        let result = drive_sub_agent_loop(
            provider,
            vec![Arc::new(OkTool)],
            Some(sec),
            None,
            &mut memory,
        )
        .await;
        let err = result.expect_err("max_tool_calls limit must abort the loop");
        assert!(err.to_string().contains("max tool calls"), "got: {err}");
    }

    /// T12：递归委派工具能力门生效——缺 CommandExecute 时 `delegate` 工具被
    /// 阻断（fail-closed，不执行）。对齐 DelegateTool 的 enforce_capability。
    #[tokio::test]
    async fn recursive_delegate_tool_requires_command_execute_capability() {
        use crate::test_utils::MockProvider;

        struct DelegateToolStub;
        #[async_trait::async_trait]
        impl Tool for DelegateToolStub {
            fn schema(&self) -> deepseeknova_core::ToolSchema {
                deepseeknova_core::ToolSchema {
                    name: "delegate".to_string(),
                    description: "recursive delegate stub".to_string(),
                    parameters: serde_json::json!({}),
                }
            }
            async fn execute(
                &self,
                _ctx: &ToolContext,
                _args: &str,
            ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
                Ok("delegate ran".to_string())
            }
        }
        let provider = Arc::new(MockProvider::sequential(vec![
            vec![
                Chunk::ToolCallStart {
                    id: "d1".into(),
                    name: "delegate".into(),
                },
                Chunk::ToolCallEnd {
                    id: "d1".into(),
                    name: "delegate".into(),
                    arguments: "{}".into(),
                },
                Chunk::Done,
            ],
            vec![
                Chunk::TextDelta("done".into()),
                Chunk::Usage(Usage::default()),
                Chunk::Done,
            ],
        ]));
        // 只授 FileRead，剥夺 CommandExecute（委派隐含命令执行）。
        let mut sec = deepseeknova_security::context::SecurityContext::with_safe_defaults();
        sec.capabilities = {
            let mut c = std::collections::HashSet::new();
            c.insert(deepseeknova_security::capability::Capability::FileRead);
            c
        };

        let mut memory = Memory::new();
        let result = drive_sub_agent_loop(
            provider,
            vec![Arc::new(DelegateToolStub)],
            Some(sec),
            None,
            &mut memory,
        )
        .await;
        assert!(result.is_ok(), "loop must complete: {result:?}");
        let msgs = memory.get_all();
        let tool_msg = msgs
            .iter()
            .find(|m| m.role == Role::Tool)
            .expect("tool result must be stored in memory");
        assert!(
            tool_msg.content.contains("CommandExecute"),
            "delegate tool must be capability-gated: {}",
            tool_msg.content
        );
        assert!(
            !tool_msg.content.contains("delegate ran"),
            "delegate tool must NOT execute without CommandExecute"
        );
    }
}
