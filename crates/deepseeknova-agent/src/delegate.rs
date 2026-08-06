//! # DelegateEngine — 模型自主 spawn 子代理（Claude Code Task-tool 式）
//!
//! 子代理是受限工具集的 [`Agent`] 实例：独立上下文、真正执行工具、只回传封顶摘要、
//! 工具集不含 `delegate`（禁递归）。并发受信号量限制，满员时排队等待。
//!
//! 任务书：每个预设持有 [`TaskSpec`]（可参数化 + RULES 约束）。inputs 来源为
//! delegate 工具调用方显式传值（见 `deepseeknova_tools::delegate`）；渲染结果
//! （task + RULES）合并进 `RunInput.prompt` —— 子 Agent 的 system_prompt 在
//! 构造期注入，无法按次渲染。

use crate::agent::Agent;
use crate::attribution::{
    compose_retry_feedback, run_attribution, AttributionBudget, AttributionSettings, Verdict,
    MAX_ATTRIBUTIONS_PER_RUN, MAX_ATTRIBUTION_INPUT_CHARS,
};
use crate::task_spec::{InputValues, TaskSpec};
use deepseeknova_core::{RunEvent, RunInput, Runner};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio_stream::StreamExt;

/// 一个内置子代理预设。`spec.tools` 为工具 schema 名白名单（均不含 "delegate"）。
#[derive(Debug, Clone)]
pub struct DelegatePreset {
    pub name: String,
    /// 角色身份 prompt，构造期注入 System 消息（与任务内容分离）。
    pub system_prompt: String,
    /// 任务书：任务文本（支持 `${{ inputs.x }}` 占位符）、RULES、工具白名单与步数上限。
    pub spec: TaskSpec,
    /// 配置层默认参数值（`[delegate] agents[].inputs`），调用方传值优先。
    pub config_inputs: InputValues,
}

impl DelegatePreset {
    /// 无参数化任务的便捷构造（task/rules/inputs 全空）。
    pub fn simple(
        name: impl Into<String>,
        system_prompt: impl Into<String>,
        tools: Vec<String>,
        max_steps: usize,
    ) -> Self {
        let name = name.into();
        Self {
            name: name.clone(),
            system_prompt: system_prompt.into(),
            spec: TaskSpec::simple(name, "", tools, max_steps),
            config_inputs: InputValues::new(),
        }
    }
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// 4 个内置预设。工具名对应真实 schema 名（read_file/bash/search_code…）；均不含 delegate。
pub fn builtin_presets() -> Vec<DelegatePreset> {
    vec![
        DelegatePreset::simple(
            "explorer",
            "You are an explorer sub-agent operating in the Observe phase of the \
                Observe → Plan → Tool → Verify → Reflect → Next Action loop. Investigate and \
                locate relevant code/facts read-only. Prefer graph tools \
                (search_code/traverse_graph/retrieve_entity) over full-file reads. Output \
                contract: return a concise findings summary with file:line evidence.",
            names(&[
                "read_file",
                "ls",
                "glob",
                "grep",
                "search_code",
                "traverse_graph",
                "retrieve_entity",
                "recall",
                "web_fetch",
            ]),
            10,
        ),
        DelegatePreset::simple(
            "coder",
            "You are a coder sub-agent operating in the Tool phase of the \
                Observe → Plan → Tool → Verify → Reflect → Next Action loop. Implement the \
                requested change: read, edit/write files, run shell as needed. Output contract: \
                return a concise summary of what changed.",
            names(&[
                "read_file",
                "write_file",
                "edit_file",
                "move_file",
                "ls",
                "glob",
                "grep",
                "bash",
                "search_code",
                "traverse_graph",
                "retrieve_entity",
            ]),
            15,
        ),
        DelegatePreset::simple(
            "tester",
            "You are a tester sub-agent operating in the Verify phase of the \
                Observe → Plan → Tool → Verify → Reflect → Next Action loop. Run tests / \
                reproduce issues via shell and report results concisely. Do not modify source \
                files.",
            names(&["read_file", "ls", "glob", "grep", "bash"]),
            10,
        ),
        DelegatePreset::simple(
            "reviewer",
            "You are a reviewer sub-agent operating in the Reflect phase of the \
                Observe → Plan → Tool → Verify → Reflect → Next Action loop. Review code \
                read-only and report issues concisely. Do not modify files.",
            names(&[
                "read_file",
                "ls",
                "glob",
                "grep",
                "search_code",
                "traverse_graph",
                "retrieve_entity",
            ]),
            10,
        ),
    ]
}

/// 委派引擎：持有每个预设一个配置好的 [`Agent`]，并发受信号量限制。
pub struct DelegateEngine {
    agents: HashMap<String, Arc<Agent>>,
    /// 预设任务书注册表（agent 名 → (spec, config_inputs)）。未注册的子代理
    /// 按旧行为处理：prompt 即 goal 原样，不做渲染。
    specs: HashMap<String, (TaskSpec, InputValues)>,
    semaphore: Arc<Semaphore>,
    output_cap_tokens: usize,
    /// 失败归因设置；None = 关闭（旧行为：失败直接上抛，无重试）。
    attribution: Option<Arc<AttributionSettings>>,
    /// 归因预算（跨委派调用累计，防烧 token）。
    attributions_used: Arc<AttributionBudget>,
}

/// 单次委派尝试的错误分类：区分渲染类与执行类。
///
/// - [`DelegateError::Render`]：任务书渲染失败（缺 required 输入、类型非法、
///   未知占位符）——确定性错误，重试必错；直接上抛，不消耗归因预算、
///   不触发 LLM 归因。
/// - [`DelegateError::Execute`]：子代理执行失败（查找 / 运行 / 收集失败），
///   可进入归因 → Retry/Degrade/Abort 流程。
enum DelegateError {
    Render(anyhow::Error),
    Execute(anyhow::Error),
}

impl DelegateError {
    fn into_anyhow(self) -> anyhow::Error {
        match self {
            DelegateError::Render(e) | DelegateError::Execute(e) => e,
        }
    }
}

impl DelegateEngine {
    pub fn new(
        agents: HashMap<String, Arc<Agent>>,
        max_concurrent: usize,
        output_cap_tokens: usize,
    ) -> Self {
        Self {
            agents,
            specs: HashMap::new(),
            semaphore: Arc::new(Semaphore::new(max_concurrent.max(1))),
            output_cap_tokens,
            attribution: None,
            attributions_used: Arc::new(AttributionBudget::new(MAX_ATTRIBUTIONS_PER_RUN)),
        }
    }

    /// 启用失败归因重试：子代理失败 → LLM 归因 → `Retry`（追加反馈重试，
    /// 受 max_retries 约束）/ `Degrade`（按 degrade_map 换 preset，未映射时
    /// 按 Retry 处理）/ `Abort`（直接上抛）；归因失败（调用/解析失败）同样
    /// 按 Abort 上抛（不阻塞、不猜）；预算超限后不再归因，走盲重试。
    /// `max_attributions = 0` 时归因关闭（仅盲重试，不调用 LLM）。
    pub fn with_attribution(mut self, settings: AttributionSettings) -> Self {
        self.attributions_used = Arc::new(AttributionBudget::new(settings.max_attributions));
        self.attribution = Some(Arc::new(settings));
        self
    }

    /// 注册一个预设的任务书与配置层默认参数值（构建 Agent 时同步调用）。
    pub fn register_spec(&mut self, name: String, spec: TaskSpec, config_inputs: InputValues) {
        self.specs.insert(name, (spec, config_inputs));
    }

    /// 已注册的子代理名（供工具做友好错误提示）。
    pub fn agent_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.agents.keys().cloned().collect();
        v.sort();
        v
    }

    /// 委派一个子代理执行 goal，返回封顶后的结果摘要。
    /// 信号量满时 **排队等待**（不拒绝）。
    ///
    /// 兼容签名：等价于 `run_with_inputs(agent, goal, InputValues::new())`，
    /// 未传参数值时行为与引入 TaskSpec 前完全一致（内置 4 preset 零变化）。
    pub async fn run(&self, agent: &str, goal: &str) -> anyhow::Result<String> {
        self.run_with_inputs(agent, goal, InputValues::new()).await
    }

    /// 带参数值委派：`values` 为本次调用的参数值；与注册的 `config_inputs`
    /// 合并（调用方传值优先），渲染任务书后随 goal 一并作为 `RunInput.prompt`。
    /// 未注册 spec 的子代理按旧行为处理：prompt 即 goal 原样。
    /// 渲染失败（如缺 required 输入）直接返回 Err（不进归因循环）。
    ///
    /// prompt 构成：goal 为基底，非空渲染段（task / rules）以 `\n\n` 追加。
    /// 简单预设（无 inputs、task 为空）渲染后 prompt 即 goal 本身，保持旧行为。
    ///
    /// 失败归因重试（需先 `with_attribution`，否则旧行为不变）：子代理失败 →
    /// 预算内 LLM 归因 → `Retry`：错误 + root_cause + fix_plan 追加为反馈消息
    /// 重新委派（受 max_retries 约束）；`Degrade`：按 degrade_map 换 preset
    /// 重试（未映射按 Retry）；`Abort` / 归因失败：直接上抛。预算超限后不再
    /// 归因，走盲重试（仅错误文本作反馈）。反馈追加在渲染段之后（最新指令）。
    /// 渲染类错误（缺 required 输入等）属确定性错误，重试必错：直接上抛，
    /// 不消耗归因预算、不触发 LLM 归因。
    pub async fn run_with_inputs(
        &self,
        agent: &str,
        goal: &str,
        values: InputValues,
    ) -> anyhow::Result<String> {
        // 无归因设置：维持旧行为（失败直接上抛，不重试）。
        let Some(cfg) = self.attribution.as_ref() else {
            return self
                .delegate_once(agent, goal, &values)
                .await
                .map_err(DelegateError::into_anyhow);
        };

        // 归因重试循环：首次尝试 + max_retries 次重试，共 max_retries+1 次。
        // 失败 → 归因分流（Retry/Degrade → 反馈重试；Abort/归因失败 → 上抛）；
        // 预算超限 → 盲重试（反馈仅错误文本，不再归因）。
        // 渲染类错误不进入本循环：确定性错误，重试必错，直接上抛。
        let mut current_agent = agent.to_string();
        let mut feedback: Option<String> = None;
        let mut attempts_left = cfg.max_retries.saturating_add(1);
        loop {
            let prompt = match &feedback {
                Some(fb) => format!("{goal}\n\n{fb}"),
                None => goal.to_string(),
            };
            match self.delegate_once(&current_agent, &prompt, &values).await {
                Ok(text) => return Ok(text),
                // 渲染类错误：参数问题，重试必错 → 直接上抛，不耗预算、不归因。
                Err(DelegateError::Render(e)) => return Err(e),
                Err(DelegateError::Execute(e)) => {
                    let err_str = e.to_string();
                    attempts_left = attempts_left.saturating_sub(1);
                    if attempts_left == 0 {
                        return Err(e);
                    }
                    if self.attributions_used.try_consume() {
                        match run_attribution(
                            cfg.provider.as_ref(),
                            goal,
                            &err_str,
                            MAX_ATTRIBUTION_INPUT_CHARS,
                        )
                        .await
                        {
                            Some(a) => match a.verdict {
                                Verdict::Retry => {
                                    feedback = Some(compose_retry_feedback(&err_str, &a));
                                }
                                Verdict::Degrade => {
                                    feedback = Some(compose_retry_feedback(&err_str, &a));
                                    if let Some(target) = cfg.degrade_map.get(&current_agent) {
                                        current_agent = target.clone();
                                    }
                                }
                                Verdict::Abort => return Err(e),
                            },
                            // 归因失败（调用/解析失败）：Abort 兜底，不阻塞、不猜。
                            None => return Err(e),
                        }
                    } else {
                        // 预算超限：盲重试（不再消耗 token 做归因）。
                        feedback = Some(format!("Previous attempt error:\n{err_str}"));
                    }
                }
            }
        }
    }

    /// 单次委派尝试（无归因重试）：渲染任务书 → 信号量排队 → 收集封顶文本。
    /// 渲染类错误（`spec.render` 失败）与执行类错误分开标记，供调用方决定
    /// 是否进入归因循环；`collect_final_text` 透传子代理 run_stream 的 Err。
    async fn delegate_once(
        &self,
        agent: &str,
        goal: &str,
        values: &InputValues,
    ) -> Result<String, DelegateError> {
        let sub = self.agents.get(agent).cloned().ok_or_else(|| {
            DelegateError::Execute(anyhow::anyhow!("unknown sub-agent '{agent}'"))
        })?;

        // 渲染任务书：本次调用值优先，config 默认值仅补缺。
        let mut prompt = goal.to_string();
        if let Some((spec, config_inputs)) = self.specs.get(agent) {
            let rendered = match spec.render(&values.merged_with(config_inputs)) {
                Ok(r) => r,
                Err(e) => return Err(DelegateError::Render(e.into())),
            };
            for part in [rendered.task, rendered.rules]
                .into_iter()
                .filter(|p| !p.is_empty())
            {
                prompt.push_str("\n\n");
                prompt.push_str(&part);
            }
        }

        let _permit =
            self.semaphore.acquire().await.map_err(|_| {
                DelegateError::Execute(anyhow::anyhow!("delegate semaphore closed"))
            })?;

        let input = RunInput {
            prompt,
            images: vec![],
            model_override: None,
        };
        let text = collect_final_text(sub.as_ref(), input)
            .await
            .map_err(DelegateError::Execute)?;
        Ok(cap_output(&text, self.output_cap_tokens))
    }
}

/// 驱动子 Agent 的 run_stream 并收集最终文本（与 CLI/serve 收集方式一致）。
/// 返回前做**输出净化**：中和子代理产出中的权限修改指令形状
/// （`permissions.allow` / `bypassPermissions` / `<settings-json` 等），
/// 防止被父模型当作可执行指令——这是子代理 → 父上下文的唯一收口点。
async fn collect_final_text(agent: &Agent, input: RunInput) -> anyhow::Result<String> {
    let mut stream = agent.run_stream(input).await?;
    let mut final_text = String::new();
    while let Some(ev) = stream.next().await {
        match ev? {
            RunEvent::TextDelta(t) => final_text.push_str(&t),
            RunEvent::Done(out) if !out.text.is_empty() => {
                final_text = out.text;
            }
            // 协议增强（阶段3）：协议事件不参与最终文本收集。
            RunEvent::PhaseTransition { .. }
            | RunEvent::GateViolation(_)
            | RunEvent::DriftFinding(_) => {}
            _ => {}
        }
    }
    let sanitized = deepseeknova_security::sanitize::sanitize_output(&final_text);
    if sanitized != final_text {
        tracing::warn!("delegate output sanitized: permission-override shape(s) neutralized");
    }
    Ok(sanitized)
}

/// 头尾截断到 token 预算（chars ≈ tokens×4），中部省略。
fn cap_output(text: &str, cap_tokens: usize) -> String {
    // P3.1：按文本自身构成换算字符预算（纯 ASCII ≈ tokens×4，纯 CJK ≈ tokens）。
    let cap_chars = crate::tokens::char_budget_for_tokens(text, cap_tokens as u32);
    let total = text.chars().count();
    if total <= cap_chars {
        return text.to_string();
    }
    let head: String = text.chars().take(cap_chars * 2 / 3).collect();
    let tail_n = cap_chars / 3;
    let tail: String = text.chars().skip(total.saturating_sub(tail_n)).collect();
    format!("{head}\n…[delegate output truncated]…\n{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::MockProvider;
    use deepseeknova_core::chunk::{Chunk, ChunkStream, Usage};
    use deepseeknova_core::{Message, Role};
    use deepseeknova_provider::{Provider, ValidatedRequest};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    #[test]
    fn presets_never_include_delegate_tool() {
        // 禁递归的可测形式：任何预设的工具集都不含 "delegate"。
        for p in builtin_presets() {
            assert!(
                !p.spec.tools.iter().any(|t| t == "delegate"),
                "preset {} must not include delegate",
                p.name
            );
        }
    }

    #[test]
    fn presets_cover_four_roles() {
        let names: Vec<String> = builtin_presets().into_iter().map(|p| p.name).collect();
        for expected in ["explorer", "coder", "tester", "reviewer"] {
            assert!(
                names.iter().any(|n| n == expected),
                "missing preset {expected}"
            );
        }
    }

    #[test]
    fn presets_keep_tool_contracts() {
        let expected: std::collections::HashMap<&str, &[&str]> = [
            (
                "explorer",
                &[
                    "read_file",
                    "ls",
                    "glob",
                    "grep",
                    "search_code",
                    "traverse_graph",
                    "retrieve_entity",
                    "recall",
                    "web_fetch",
                ][..],
            ),
            (
                "coder",
                &[
                    "read_file",
                    "write_file",
                    "edit_file",
                    "move_file",
                    "ls",
                    "glob",
                    "grep",
                    "bash",
                    "search_code",
                    "traverse_graph",
                    "retrieve_entity",
                ][..],
            ),
            ("tester", &["read_file", "ls", "glob", "grep", "bash"][..]),
            (
                "reviewer",
                &[
                    "read_file",
                    "ls",
                    "glob",
                    "grep",
                    "search_code",
                    "traverse_graph",
                    "retrieve_entity",
                ][..],
            ),
        ]
        .into_iter()
        .collect();
        for p in builtin_presets() {
            let want: Vec<String> = expected[p.name.as_str()]
                .iter()
                .map(|s| s.to_string())
                .collect();
            assert_eq!(
                p.spec.tools, want,
                "preset {} tool contract changed",
                p.name
            );
        }
    }

    #[test]
    fn cap_output_truncates_long_and_keeps_short() {
        let long = "x".repeat(10_000);
        let out = cap_output(&long, 100);
        assert!(out.chars().count() < 10_000);
        assert!(out.contains("truncated"));
        assert_eq!(cap_output("hello", 100), "hello");
    }

    #[tokio::test]
    async fn run_delegates_to_agent_and_caps() {
        let mut agents: HashMap<String, Arc<Agent>> = HashMap::new();
        let sub = Agent::new(
            Arc::new(MockProvider::text("explored: found the bug in auth.rs")),
            3,
        )
        .with_system_prompt("explorer");
        agents.insert("explorer".into(), Arc::new(sub));
        let engine = DelegateEngine::new(agents, 2, 2000);

        let out = engine.run("explorer", "find the bug").await.unwrap();
        assert!(out.contains("explored"), "got: {out}");
    }

    #[tokio::test]
    async fn run_unknown_agent_errors() {
        let engine = DelegateEngine::new(HashMap::new(), 2, 2000);
        assert!(engine.run("nope", "x").await.is_err());
    }

    /// 构造一个注册了参数化任务书的引擎：spec 要求 `path`（required），
    /// 并有规则条目；config 层提供 `path` 默认值。
    fn engine_with_spec() -> DelegateEngine {
        let spec = TaskSpec {
            name: "reviewer".into(),
            task: "Review ${{ inputs.path }}".into(),
            rules: vec!["Do not modify files".into()],
            inputs: vec![crate::task_spec::InputSpec {
                name: "path".into(),
                ty: crate::task_spec::InputType::String,
                required: true,
                default: None,
            }],
            tools: vec!["read_file".into()],
            max_steps: 10,
        };
        let mut agents: HashMap<String, Arc<Agent>> = HashMap::new();
        agents.insert(
            "reviewer".into(),
            Arc::new(
                Agent::new(Arc::new(MockProvider::text("reviewed")), 3)
                    .with_system_prompt("reviewer"),
            ),
        );
        let mut engine = DelegateEngine::new(agents, 2, 2000);
        engine.register_spec(
            "reviewer".into(),
            spec,
            InputValues::from(HashMap::from([("path".to_string(), "lib.rs".to_string())])),
        );
        engine
    }

    #[tokio::test]
    async fn run_render_error_propagates() {
        // 无 config 默认值 + 无调用方传值 → 缺 required 输入 → Err。
        let spec = engine_with_spec().specs.remove("reviewer").unwrap().0;
        let mut agents: HashMap<String, Arc<Agent>> = HashMap::new();
        agents.insert(
            "reviewer".into(),
            Arc::new(
                Agent::new(Arc::new(MockProvider::text("reviewed")), 3)
                    .with_system_prompt("reviewer"),
            ),
        );
        let mut engine = DelegateEngine::new(agents, 2, 2000);
        engine.register_spec("reviewer".into(), spec, InputValues::new());

        let err = engine
            .run_with_inputs("reviewer", "go", InputValues::new())
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("missing required input 'path'"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn run_config_inputs_supply_required() {
        // config 默认值补上 required 输入后，渲染成功（无调用方传值）。
        let engine = engine_with_spec();
        let out = engine
            .run_with_inputs("reviewer", "go", InputValues::new())
            .await
            .unwrap();
        assert!(out.contains("reviewed"), "got: {out}");
    }

    #[tokio::test]
    async fn run_with_inputs_renders_values_into_prompt() {
        // 注册带 inputs 的 spec（无 config 默认值），调用方传值 → 渲染进 prompt：
        // 子 Agent 收到的 user prompt = goal + 渲染后 task + RULES。
        let provider = Arc::new(MockProvider::text("reviewed"));
        let mut agents: HashMap<String, Arc<Agent>> = HashMap::new();
        agents.insert(
            "reviewer".into(),
            Arc::new(Agent::new(provider.clone(), 3).with_system_prompt("reviewer")),
        );
        let mut engine = DelegateEngine::new(agents, 2, 2000);
        engine.register_spec(
            "reviewer".into(),
            TaskSpec {
                name: "reviewer".into(),
                task: "Review ${{ inputs.path }}".into(),
                rules: vec!["Do not modify files".into()],
                inputs: vec![crate::task_spec::InputSpec {
                    name: "path".into(),
                    ty: crate::task_spec::InputType::String,
                    required: true,
                    default: None,
                }],
                tools: vec!["read_file".into()],
                max_steps: 10,
            },
            InputValues::new(),
        );

        let out = engine
            .run_with_inputs(
                "reviewer",
                "go",
                InputValues::from(HashMap::from([(
                    "path".to_string(),
                    "src/lib.rs".to_string(),
                )])),
            )
            .await
            .unwrap();
        assert!(out.contains("reviewed"), "got: {out}");

        let prompt = provider.last_prompt().unwrap();
        assert!(prompt.contains("go"), "goal 应保留: {prompt}");
        assert!(
            prompt.contains("Review src/lib.rs"),
            "占位符应替换为传值: {prompt}"
        );
        assert!(prompt.contains("## RULES"), "RULES 块应注入: {prompt}");
        assert!(
            prompt.contains("Do not modify files"),
            "规则文本应注入: {prompt}"
        );
    }

    // -----------------------------------------------------------------------
    // 失败归因重试（设计 B）
    // -----------------------------------------------------------------------

    /// 子代理 provider：前 `fail_times` 次 stream 调用返回 Err，之后成功。
    /// 记录调用次数与最后一次用户消息（断言重试反馈内容）。
    struct FlakyProvider {
        remaining_failures: AtomicUsize,
        calls: AtomicUsize,
        last_prompt: Mutex<Option<String>>,
    }

    impl FlakyProvider {
        fn new(fail_times: usize) -> Self {
            Self {
                remaining_failures: AtomicUsize::new(fail_times),
                calls: AtomicUsize::new(0),
                last_prompt: Mutex::new(None),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn last_prompt(&self) -> Option<String> {
            self.last_prompt.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl Provider for FlakyProvider {
        async fn generate(&self, _v: ValidatedRequest<'_>) -> anyhow::Result<Message> {
            Ok(Message {
                role: Role::Assistant,
                content: "ok".into(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            })
        }

        async fn stream(&self, v: ValidatedRequest<'_>) -> anyhow::Result<ChunkStream> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(last) = v.messages.iter().rev().find(|m| m.role == Role::User) {
                *self.last_prompt.lock().unwrap() = Some(last.content.clone());
            }
            let should_fail = self
                .remaining_failures
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |r| {
                    (r > 0).then(|| r - 1) // 惰性：r==0 时不计算 r-1（then_some 参数急切求值会溢出）
                })
                .is_ok();
            if should_fail {
                anyhow::bail!("sub-agent crashed");
            }
            let chunks: Vec<anyhow::Result<Chunk>> = vec![
                Ok(Chunk::TextDelta("explored ok".into())),
                Ok(Chunk::Usage(Usage::default())),
                Ok(Chunk::Done),
            ];
            Ok(Box::pin(tokio_stream::iter(chunks)))
        }
    }

    /// 归因用 generate-only provider：依次弹出 JSON 响应，单响应时重复
    /// （与 MockProvider 的 stream 语义一致；归因路径只走 generate）。
    struct VerdictProvider {
        responses: Mutex<Vec<String>>,
        calls: AtomicUsize,
    }

    impl VerdictProvider {
        fn new(responses: Vec<&str>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().map(str::to_string).collect()),
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl Provider for VerdictProvider {
        async fn generate(&self, _v: ValidatedRequest<'_>) -> anyhow::Result<Message> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut lock = self.responses.lock().unwrap();
            let content = if lock.len() > 1 {
                lock.remove(0)
            } else {
                lock.first().cloned().unwrap_or_default()
            };
            Ok(Message {
                role: Role::Assistant,
                content,
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            })
        }

        async fn stream(&self, _v: ValidatedRequest<'_>) -> anyhow::Result<ChunkStream> {
            anyhow::bail!("VerdictProvider is generate-only")
        }
    }

    /// 按 (name, provider) 列表建引擎，每个子代理都是独立 FlakyProvider。
    fn engine(agents: &[(&str, Arc<FlakyProvider>)]) -> DelegateEngine {
        let map: HashMap<String, Arc<Agent>> = agents
            .iter()
            .map(|(n, p)| {
                (
                    n.to_string(),
                    Arc::new(Agent::new(p.clone(), 3).with_system_prompt(*n)),
                )
            })
            .collect();
        DelegateEngine::new(map, 2, 2000)
    }

    #[tokio::test]
    async fn retry_succeeds_after_two_failures() {
        // 子代理失败 2 次后成功；每次失败归因都判定 Retry → 反馈重试。
        let sub = Arc::new(FlakyProvider::new(2));
        let attrib = Arc::new(VerdictProvider::new(vec![
            r#"{"root_cause":"transient","verdict":"retry","fix_plan":"just retry"}"#,
        ]));
        let engine = engine(&[("coder", sub.clone())]).with_attribution(AttributionSettings {
            provider: attrib.clone(),
            max_retries: 3,
            max_attributions: 5,
            degrade_map: HashMap::new(),
        });

        let out = engine.run("coder", "find the bug").await.unwrap();
        assert!(out.contains("explored ok"), "got: {out}");
        assert_eq!(sub.calls(), 3, "2 次失败 + 1 次成功");
        assert_eq!(attrib.calls(), 2, "每次失败都归因一次");

        // 最后一次尝试的 prompt 必须带归因反馈（错误 + root_cause + fix_plan）。
        let last = sub.last_prompt().unwrap();
        assert!(last.contains("find the bug"), "goal 应保留: {last}");
        assert!(last.contains("root cause: transient"), "got: {last}");
        assert!(last.contains("fix plan: just retry"), "got: {last}");
        assert!(last.contains("Previous attempt error"), "got: {last}");
        assert!(last.contains("sub-agent crashed"), "错误文本应保留: {last}");
    }

    #[tokio::test]
    async fn render_error_skips_attribution_and_budget() {
        // 渲染失败（缺 required 输入）→ 直接上抛：不调用归因 provider、
        // 不消耗归因预算、子代理不被执行（FlakyProvider 零调用）。
        let spec = TaskSpec {
            name: "reviewer".into(),
            task: "Review ${{ inputs.path }}".into(),
            rules: vec![],
            inputs: vec![crate::task_spec::InputSpec {
                name: "path".into(),
                ty: crate::task_spec::InputType::String,
                required: true,
                default: None,
            }],
            tools: vec!["read_file".into()],
            max_steps: 10,
        };
        let sub = Arc::new(FlakyProvider::new(usize::MAX));
        let mut agents: HashMap<String, Arc<Agent>> = HashMap::new();
        agents.insert(
            "reviewer".into(),
            Arc::new(Agent::new(sub.clone(), 3).with_system_prompt("reviewer")),
        );
        let attrib = Arc::new(VerdictProvider::new(vec![
            r#"{"root_cause":"x","verdict":"retry","fix_plan":"y"}"#,
        ]));
        let mut engine = DelegateEngine::new(agents, 2, 2000);
        engine.register_spec("reviewer".into(), spec, InputValues::new());
        let engine = engine.with_attribution(AttributionSettings {
            provider: attrib.clone(),
            max_retries: 3,
            max_attributions: 5,
            degrade_map: HashMap::new(),
        });

        let err = engine
            .run_with_inputs("reviewer", "go", InputValues::new())
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("missing required input 'path'"),
            "应上抛渲染错误: {err}"
        );
        assert_eq!(attrib.calls(), 0, "渲染错误不得触发归因");
        assert_eq!(sub.calls(), 0, "渲染错误不得执行子代理");
    }

    #[tokio::test]
    async fn attribution_budget_exhausted_blind_retries_then_errors() {
        // 预算 1：第一次失败归因（Retry），后续失败不再归因，走盲重试；
        // max_retries 耗尽后上抛最后错误。
        let sub = Arc::new(FlakyProvider::new(usize::MAX));
        let attrib = Arc::new(VerdictProvider::new(vec![
            r#"{"root_cause":"x","verdict":"retry","fix_plan":"y"}"#,
        ]));
        let engine = engine(&[("coder", sub.clone())]).with_attribution(AttributionSettings {
            provider: attrib.clone(),
            max_retries: 2,
            max_attributions: 1,
            degrade_map: HashMap::new(),
        });

        let err = engine.run("coder", "go").await.unwrap_err();
        assert!(err.to_string().contains("sub-agent crashed"), "got: {err}");
        assert_eq!(sub.calls(), 3, "首次 + 2 次重试（预算耗尽后盲重试）");
        assert_eq!(attrib.calls(), 1, "预算超限后不得再归因");
    }

    #[tokio::test]
    async fn degrade_switches_preset() {
        // coder 永远失败、explorer 成功；归因判定 Degrade → 按 degrade_map
        // 换 preset 重试（带归因反馈）。
        let coder = Arc::new(FlakyProvider::new(usize::MAX));
        let explorer = Arc::new(FlakyProvider::new(0));
        let attrib = Arc::new(VerdictProvider::new(vec![
            r#"{"root_cause":"wrong role","verdict":"degrade","fix_plan":"use explorer"}"#,
        ]));
        let engine = engine(&[("coder", coder.clone()), ("explorer", explorer.clone())])
            .with_attribution(AttributionSettings {
                provider: attrib.clone(),
                max_retries: 2,
                max_attributions: 3,
                degrade_map: HashMap::from([("coder".to_string(), "explorer".to_string())]),
            });

        let out = engine.run("coder", "explore the codebase").await.unwrap();
        assert!(out.contains("explored ok"), "got: {out}");
        assert_eq!(coder.calls(), 1, "coder 失败一次后不再使用");
        assert_eq!(explorer.calls(), 1, "降级目标 explorer 被调用");

        // 降级重试同样带归因反馈（错误 + root_cause + fix_plan）。
        let last = explorer.last_prompt().unwrap();
        assert!(last.contains("wrong role"), "got: {last}");
        assert!(last.contains("use explorer"), "got: {last}");
        assert!(last.contains("sub-agent crashed"), "got: {last}");
    }

    #[tokio::test]
    async fn abort_verdict_propagates_error_without_retry() {
        let sub = Arc::new(FlakyProvider::new(usize::MAX));
        let attrib = Arc::new(VerdictProvider::new(vec![
            r#"{"root_cause":"impossible","verdict":"abort"}"#,
        ]));
        let engine = engine(&[("coder", sub.clone())]).with_attribution(AttributionSettings {
            provider: attrib.clone(),
            max_retries: 3,
            max_attributions: 3,
            degrade_map: HashMap::new(),
        });

        let err = engine.run("coder", "go").await.unwrap_err();
        assert!(err.to_string().contains("sub-agent crashed"), "got: {err}");
        assert_eq!(sub.calls(), 1, "Abort 判定不得重试");
        assert_eq!(attrib.calls(), 1);
    }

    #[tokio::test]
    async fn unparseable_attribution_defaults_to_abort() {
        // 归因响应不可解析 → Abort 兜底（不阻塞、不猜），直接上抛。
        let sub = Arc::new(FlakyProvider::new(usize::MAX));
        let engine = engine(&[("coder", sub.clone())]).with_attribution(AttributionSettings {
            provider: Arc::new(VerdictProvider::new(vec!["I'll look into it"])),
            max_retries: 3,
            max_attributions: 3,
            degrade_map: HashMap::new(),
        });

        let err = engine.run("coder", "go").await.unwrap_err();
        assert!(err.to_string().contains("sub-agent crashed"), "got: {err}");
        assert_eq!(sub.calls(), 1, "归因失败 = Abort，不重试");
    }

    #[tokio::test]
    async fn no_attribution_config_keeps_old_behavior() {
        // 未启用归因：失败直接上抛（与引入重试前行为一致）。
        let sub = Arc::new(FlakyProvider::new(usize::MAX));
        let engine = engine(&[("coder", sub.clone())]);

        let err = engine.run("coder", "go").await.unwrap_err();
        assert!(err.to_string().contains("sub-agent crashed"), "got: {err}");
        assert_eq!(sub.calls(), 1, "未启用归因时失败直接上抛");
    }
}
