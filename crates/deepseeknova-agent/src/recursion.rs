//! # 子代理递归：DelegationSink / 深度扩展 / 递归委派工具
//!
//! 放开"禁递归"后，子代理可再派子代理，但受**深度上限**约束（默认 3）。
//! 本模块提供：
//!
//! - [`DelegationSink`]：派发出口抽象。`SubAgentRunner` 与 `DelegateEngine`
//!   各自实现，把"再派一个子代理"统一为一次带深度参数的调用。
//! - [`DelegateDepth`]：当前调用深度扩展，由子代理执行循环注入每个
//!   `deepseeknova_core::tool::ToolContext`，供递归委派工具读取。
//! - [`RecursiveDelegateTool`]：agent crate 自带的递归委派工具实现
//!   （`deepseeknova-core::Tool`）。读当前深度 → 校验上限 → 经
//!   [`DelegationSink`] 派发 `depth + 1`。超深时优雅降级（返回错误文本给
//!   模型，不硬失败）。sink 来源：优先工具构造时注入，否则从 ToolContext
//!   的 `Arc<dyn DelegationSink>` 扩展读取（子代理循环注入），避免
//!   runner/engine 与工具的循环构造。
//!
//! 深度语义：根派发（主 agent / coordinator 直接调 `run` / `run_stream`）
//! 为 depth 1；递归委派工具内部把 `depth` 传给 sink，sink 入口校验
//! `depth <= max_depth`，超限即拒绝。
//!
//! 注入点：SubAgentRunner 的子代理执行循环按实际深度注入；Agent 主循环
//! （`build_tool_context`）以根深度 1 注入（主循环不嵌套，恒 1）。本 crate
//! 的 [`crate::delegate_tool::DelegateTool`]（主 agent 的 delegate 工具）同样
//! 读本扩展后按 depth+1 派发到 `DelegateEngine::run_at_depth`。

use crate::task_spec::InputValues;
use async_trait::async_trait;
use deepseeknova_core::Tool;
use deepseeknova_core::{DeepseeknovaError, ToolContext, ToolSchema};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// 派发出口抽象：`SubAgentRunner` 与 `DelegateEngine` 实现，供递归委派工具
/// 把嵌套子代理调用送回各自引擎。
#[async_trait]
pub trait DelegationSink: Send + Sync {
    /// 派发一个子代理。`depth` 为**本次派发**的深度（根派发为 1）。
    /// 返回子代理最终文本（封顶语义由实现方决定）。
    async fn delegate(
        &self,
        agent: &str,
        goal: &str,
        values: &InputValues,
        depth: usize,
    ) -> Result<String, DeepseeknovaError>;

    /// 带父取消令牌的派发（T12 接线）。默认实现忽略令牌、回落到
    /// [`Self::delegate`]；支持取消传播的 sink（如 [`crate::DelegateEngine`]）
    /// 覆盖之，把父 run 的取消令牌透传到子代理执行。
    async fn delegate_with_parent_cancel(
        &self,
        agent: &str,
        goal: &str,
        values: &InputValues,
        depth: usize,
        _parent_cancel: Option<CancellationToken>,
    ) -> Result<String, DeepseeknovaError> {
        self.delegate(agent, goal, values, depth).await
    }
}

/// 当前子代理调用深度的执行期扩展。由子代理执行循环注入每个 ToolContext
/// （SubAgentRunner 按实际深度；Agent 主循环以根深度 1 注入）；
/// 未注入时递归委派工具按 depth 0 处理（根环境）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelegateDepth(pub usize);

/// 递归委派工具参数（与 tools crate 的 delegate 工具同形，便于迁移）。
#[derive(Deserialize)]
struct RecursiveDelegateArgs {
    agent: String,
    goal: String,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    inputs: Option<std::collections::HashMap<String, String>>,
}

/// 递归委派工具：子代理用它再派子代理，深度受 `max_depth` 限制。
/// 深度从 [`DelegateDepth`] 扩展读取（由子代理循环注入）；超深返回
/// 错误文本（模型可降级），不抛错。
pub struct RecursiveDelegateTool {
    max_depth: usize,
    /// 可选：构造期注入的 sink。未注入时从 ToolContext 扩展读取
    /// `Arc<dyn DelegationSink>`（子代理循环注入）。
    sink: Option<Arc<dyn DelegationSink>>,
}

impl RecursiveDelegateTool {
    /// 构造工具。`sink` 为空时从 ToolContext 扩展读取派发出口。
    pub fn new(max_depth: usize) -> Self {
        Self {
            max_depth: max_depth.max(1),
            sink: None,
        }
    }

    /// 显式注入派发出口（优先于 ToolContext 扩展）。
    pub fn with_sink(mut self, sink: Arc<dyn DelegationSink>) -> Self {
        self.sink = Some(sink);
        self
    }
}

#[async_trait]
impl Tool for RecursiveDelegateTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "delegate".to_string(),
            description:
                "Delegates a subtask to a sub-agent; recursion is allowed up to a depth limit."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "agent": {"type": "string", "description": "Agent name."},
                    "goal": {"type": "string", "description": "Goal."},
                    "context": {"type": "string", "description": "Context."},
                    "inputs": {
                        "type": "object",
                        "additionalProperties": {"type": "string"},
                        "description": "Values for task-spec placeholders (${{ inputs.x }})."
                    }
                },
                "required": ["agent", "goal"]
            }),
        }
    }

    async fn execute(
        &self,
        ctx: &ToolContext,
        args: &str,
    ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
        if ctx.cancellation.is_cancelled() {
            return Err(deepseeknova_core::DeepseeknovaError::Cancelled);
        }
        let parsed: RecursiveDelegateArgs = serde_json::from_str(args)?;
        let current = ctx
            .extensions
            .get::<DelegateDepth>()
            .map(|d| d.0)
            .unwrap_or(0);
        let next = current + 1;
        if next > self.max_depth {
            return Ok(format!(
                "Error: sub-agent recursion depth exceeded (max {}); do not delegate further",
                self.max_depth
            ));
        }
        let sink = match &self.sink {
            Some(s) => s.clone(),
            None => match ctx.extensions.get::<Arc<dyn DelegationSink>>() {
                Some(s) => s.clone(),
                None => {
                    return Ok(
                        "Error: no delegation sink available for sub-agent recursion".to_string(),
                    )
                }
            },
        };
        let goal = match parsed.context {
            Some(c) if !c.is_empty() => format!("{c}\n\n{}", parsed.goal),
            _ => parsed.goal.clone(),
        };
        let values = InputValues::from(parsed.inputs.unwrap_or_default());
        // T12：把当前子代理工具上下文的主取消令牌传给 sink——递归派发的
        // 子代理执行使用其 child_token()，父 run 取消立即中止整条派发链。
        match sink
            .delegate_with_parent_cancel(
                &parsed.agent,
                &goal,
                &values,
                next,
                Some(ctx.cancellation.clone()),
            )
            .await
        {
            Ok(text) => Ok(format!("[delegate:{}] {text}", parsed.agent)),
            Err(e) => Ok(format!(
                "delegate to '{}' failed at depth {next}: {e}",
                parsed.agent
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_core::chunk::Chunk;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 记录调用深度与次数的假 sink。
    struct RecordingSink {
        calls: AtomicUsize,
        max_seen: AtomicUsize,
        fail_over: usize,
        result: String,
    }

    impl RecordingSink {
        fn ok(result: &str) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                max_seen: AtomicUsize::new(0),
                fail_over: usize::MAX,
                result: result.to_string(),
            }
        }
    }

    #[async_trait]
    impl DelegationSink for RecordingSink {
        async fn delegate(
            &self,
            agent: &str,
            _goal: &str,
            _values: &InputValues,
            depth: usize,
        ) -> Result<String, DeepseeknovaError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut seen = self.max_seen.load(Ordering::SeqCst);
            while depth > seen {
                match self.max_seen.compare_exchange_weak(
                    seen,
                    depth,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => break,
                    Err(cur) => seen = cur,
                }
            }
            if depth >= self.fail_over {
                return Err(DeepseeknovaError::runner(format!(
                    "boom at depth {depth} for {agent}"
                )));
            }
            Ok(self.result.clone())
        }
    }

    fn ctx_with_depth(depth: usize) -> ToolContext {
        let mut ctx = ToolContext::new("t");
        ctx.extensions.insert(DelegateDepth(depth));
        ctx
    }

    fn ctx_with_depth_and_sink(depth: usize, sink: Arc<dyn DelegationSink>) -> ToolContext {
        let mut ctx = ToolContext::new("t");
        ctx.extensions.insert(DelegateDepth(depth));
        ctx.extensions.insert(sink);
        ctx
    }

    #[tokio::test]
    async fn tool_delegates_at_next_depth() {
        let sink = Arc::new(RecordingSink::ok("leaf done"));
        let tool = RecursiveDelegateTool::new(3).with_sink(sink.clone());
        let out = tool
            .execute(&ctx_with_depth(1), r#"{"agent":"leaf","goal":"finish"}"#)
            .await
            .unwrap();
        assert!(out.contains("[delegate:leaf]"), "got: {out}");
        assert!(out.contains("leaf done"));
        assert_eq!(sink.calls.load(Ordering::SeqCst), 1);
        assert_eq!(sink.max_seen.load(Ordering::SeqCst), 2, "depth 1 → 2");
    }

    #[tokio::test]
    async fn tool_graceful_when_depth_exceeded() {
        let sink = Arc::new(RecordingSink::ok("leaf"));
        let tool = RecursiveDelegateTool::new(3).with_sink(sink.clone());
        // 当前深度 3 → next 4 > 3 → 拒绝且不调用 sink
        let out = tool
            .execute(&ctx_with_depth(3), r#"{"agent":"leaf","goal":"x"}"#)
            .await
            .unwrap();
        assert!(out.contains("recursion depth exceeded"), "got: {out}");
        assert_eq!(sink.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn tool_reads_sink_from_context() {
        // 未注入 sink → 从 ToolContext 扩展读取（子代理循环的注入方式）
        let sink = Arc::new(RecordingSink::ok("leaf"));
        let tool = RecursiveDelegateTool::new(3);
        let out = tool
            .execute(
                &ctx_with_depth_and_sink(1, sink.clone()),
                r#"{"agent":"leaf","goal":"x"}"#,
            )
            .await
            .unwrap();
        assert!(out.contains("leaf"));
        assert_eq!(sink.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn tool_missing_sink_is_graceful() {
        let tool = RecursiveDelegateTool::new(3);
        let out = tool
            .execute(&ctx_with_depth(1), r#"{"agent":"leaf","goal":"x"}"#)
            .await
            .unwrap();
        assert!(out.contains("no delegation sink"), "got: {out}");
    }

    #[tokio::test]
    async fn tool_root_without_depth_uses_zero() {
        let sink = Arc::new(RecordingSink::ok("leaf"));
        let tool = RecursiveDelegateTool::new(3).with_sink(sink.clone());
        // 无 DelegateDepth 扩展 → current 0 → next 1
        let ctx = ToolContext::new("t");
        tool.execute(&ctx, r#"{"agent":"leaf","goal":"x"}"#)
            .await
            .unwrap();
        assert_eq!(sink.max_seen.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn tool_propagates_error_text() {
        let sink = Arc::new(RecordingSink {
            calls: AtomicUsize::new(0),
            max_seen: AtomicUsize::new(0),
            fail_over: 2,
            result: String::new(),
        });
        let tool = RecursiveDelegateTool::new(3).with_sink(sink);
        let out = tool
            .execute(&ctx_with_depth(1), r#"{"agent":"leaf","goal":"x"}"#)
            .await
            .unwrap();
        assert!(out.contains("failed at depth 2"), "got: {out}");
        assert!(out.contains("boom at depth 2"));
    }

    // --- 端到端：SubAgentRunner 内子代理经递归委派工具再派子代理 ---

    /// 按模型名分派不同 provider 的解析器。
    struct Resolver {
        coder: Arc<crate::test_utils::MockProvider>,
        leaf: Arc<crate::test_utils::MockProvider>,
    }
    impl crate::sub_agent::ModelResolver for Resolver {
        fn resolve(&self, name: &str) -> Option<Arc<dyn deepseeknova_provider::Provider>> {
            match name {
                "m-coder" => Some(self.coder.clone() as Arc<dyn deepseeknova_provider::Provider>),
                "m-leaf" => Some(self.leaf.clone() as Arc<dyn deepseeknova_provider::Provider>),
                _ => None,
            }
        }
    }

    #[tokio::test]
    async fn end_to_end_sub_agent_recurses_with_depth() {
        use crate::sub_agent::{SubAgentConfig, SubAgentRunner};
        use crate::task_spec::InputValues;
        use crate::test_utils::MockProvider;
        use deepseeknova_core::chunk::Usage;

        let call_id = "call_del";
        let coder_p = Arc::new(MockProvider::sequential(vec![
            vec![
                Chunk::ToolCallStart {
                    id: call_id.to_string(),
                    name: "delegate".to_string(),
                },
                Chunk::ToolCallEnd {
                    id: call_id.to_string(),
                    name: "delegate".to_string(),
                    arguments: r#"{"agent":"leaf","goal":"list files"}"#.to_string(),
                },
                Chunk::Done,
            ],
            vec![
                Chunk::TextDelta("coder finished".to_string()),
                Chunk::Usage(Usage::default()),
                Chunk::Done,
            ],
        ]));
        let leaf_p = Arc::new(MockProvider::text("leaf output"));

        let mut runner = SubAgentRunner::new(Arc::new(MockProvider::text("unused")))
            .with_max_depth(3)
            .with_model_resolver(Arc::new(Resolver {
                coder: coder_p.clone(),
                leaf: leaf_p.clone(),
            }))
            .with_default("coder");
        runner.register(
            SubAgentConfig::new("coder", "you are coder")
                .with_model(Some("m-coder".to_string()))
                .with_tools(vec![
                    Arc::new(RecursiveDelegateTool::new(3)) as Arc<dyn Tool>
                ]),
        );
        runner.register(
            SubAgentConfig::new("leaf", "you are leaf").with_model(Some("m-leaf".to_string())),
        );

        // 递归出口：把 runner 自身回填为 sink（子代理 delegate 工具经此再派）。
        let runner = Arc::new(runner);
        runner.set_delegation_sink(runner.clone());

        let out = runner
            .run_at_depth("coder", "go", &InputValues::new(), 1)
            .await
            .unwrap();
        assert!(out.contains("coder finished"), "got: {out}");
        assert!(leaf_p.call_count() >= 1, "leaf 子代理必须被递归调用");
        assert_eq!(coder_p.call_count(), 2, "coder 两轮：工具轮 + 收尾轮");
    }

    #[tokio::test]
    async fn end_to_end_depth_limit_blocks_nested_delegation() {
        use crate::sub_agent::{SubAgentConfig, SubAgentRunner};
        use crate::task_spec::InputValues;
        use crate::test_utils::MockProvider;
        use deepseeknova_core::chunk::Usage;

        // coder 在 depth 2 派发，max_depth = 2 → next 3 > 2 → 递归工具优雅拒绝
        let coder_p = Arc::new(MockProvider::sequential(vec![
            vec![
                Chunk::ToolCallStart {
                    id: "call_del".to_string(),
                    name: "delegate".to_string(),
                },
                Chunk::ToolCallEnd {
                    id: "call_del".to_string(),
                    name: "delegate".to_string(),
                    arguments: r#"{"agent":"leaf","goal":"x"}"#.to_string(),
                },
                Chunk::Done,
            ],
            vec![
                Chunk::TextDelta("coder done".to_string()),
                Chunk::Usage(Usage::default()),
                Chunk::Done,
            ],
        ]));
        let leaf_p = Arc::new(MockProvider::text("leaf output"));

        let mut runner = SubAgentRunner::new(Arc::new(MockProvider::text("unused")))
            .with_max_depth(2)
            .with_model_resolver(Arc::new(Resolver {
                coder: coder_p.clone(),
                leaf: leaf_p.clone(),
            }))
            .with_default("coder");
        runner.register(
            SubAgentConfig::new("coder", "you are coder")
                .with_model(Some("m-coder".to_string()))
                .with_tools(vec![
                    Arc::new(RecursiveDelegateTool::new(2)) as Arc<dyn Tool>
                ]),
        );
        runner.register(
            SubAgentConfig::new("leaf", "you are leaf").with_model(Some("m-leaf".to_string())),
        );
        let runner = Arc::new(runner);
        runner.set_delegation_sink(runner.clone());

        // coder 在 depth 2 派发 → 再派 leaf 被深度上限拦截
        let out = runner
            .run_at_depth("coder", "go", &InputValues::new(), 2)
            .await
            .unwrap();
        assert!(out.contains("coder done"), "got: {out}");
        assert_eq!(leaf_p.call_count(), 0, "超深时 leaf 不得被调用");
    }

    // -----------------------------------------------------------------------
    // 端到端：DelegateEngine 路径（tools DelegateTool 的引擎侧）深度传播
    // -----------------------------------------------------------------------
    //
    // 主 agent 的 delegate 工具读主循环注入的 DelegateDepth(1) → 经
    // engine.run_at_depth 以深度 2 派发 coder；coder 是引擎内的普通 Agent，
    // 其主循环同样以根深度 1 注入 DelegateDepth——为演示"子代理内再 delegate
    // 深度递增"，测试以 with_extension(DelegateDepth(2)) 模拟运行时侧把当前
    // 深度传入引擎子代理（P1-5：引擎侧自动深度传播由运行时注入）。深度链：
    // 主 agent(1) → coder(2) → leaf(3)。

    /// 晚绑定派发出口：先构造引擎子代理、再建引擎，引擎创建后回填
    /// （打破"子代理需引擎、引擎需子代理"的构造环）。
    struct LateSink(Arc<std::sync::OnceLock<Arc<dyn DelegationSink>>>);

    #[async_trait]
    impl DelegationSink for LateSink {
        async fn delegate(
            &self,
            agent: &str,
            goal: &str,
            values: &InputValues,
            depth: usize,
        ) -> Result<String, DeepseeknovaError> {
            let sink = self.0.get().expect("late sink not set");
            sink.delegate(agent, goal, values, depth).await
        }
    }

    #[tokio::test]
    async fn end_to_end_engine_recursion_increments_depth() {
        use crate::agent::Agent;
        use crate::delegate::DelegateEngine;
        use crate::test_utils::MockProvider;
        use deepseeknova_core::chunk::{Chunk, Usage};
        use deepseeknova_core::{RunEvent, RunInput, Runner};
        use std::collections::HashMap;
        use tokio_stream::StreamExt;

        let leaf_p = Arc::new(MockProvider::text("leaf output"));
        let coder_p = Arc::new(MockProvider::sequential(vec![
            vec![
                Chunk::ToolCallStart {
                    id: "call_nested".to_string(),
                    name: "delegate".to_string(),
                },
                Chunk::ToolCallEnd {
                    id: "call_nested".to_string(),
                    name: "delegate".to_string(),
                    arguments: r#"{"agent":"leaf","goal":"leaf work"}"#.to_string(),
                },
                Chunk::Done,
            ],
            vec![
                Chunk::TextDelta("coder finished".to_string()),
                Chunk::Usage(Usage::default()),
                Chunk::Done,
            ],
        ]));

        // 晚绑定引擎句柄：coder 的递归工具在引擎创建前构造，引擎建好后回填。
        let sink_slot: Arc<std::sync::OnceLock<Arc<dyn DelegationSink>>> =
            Arc::new(std::sync::OnceLock::new());
        let late = Arc::new(LateSink(sink_slot.clone()));

        let mut coder = Agent::new(coder_p.clone(), 4)
            .with_system_prompt("coder")
            // 模拟运行时把当前深度 2 传入引擎子代理工具上下文（P1-5）。
            .with_extension(DelegateDepth(2));
        coder.register_tool(
            Arc::new(RecursiveDelegateTool::new(3).with_sink(late)) as Arc<dyn Tool>,
        );
        let leaf = Agent::new(leaf_p.clone(), 3).with_system_prompt("leaf");

        let mut agents: HashMap<String, Arc<Agent>> = HashMap::new();
        agents.insert("coder".into(), Arc::new(coder));
        agents.insert("leaf".into(), Arc::new(leaf));
        let engine = Arc::new(DelegateEngine::new(agents, 2, 2000).with_max_depth(3));
        let _ = sink_slot.set(engine.clone() as Arc<dyn DelegationSink>);

        // 主 agent（root）：经引擎路径派发 coder，深度由主循环注入的
        // DelegateDepth(1) 递增到 2。
        let main_p = Arc::new(MockProvider::sequential(vec![
            vec![
                Chunk::ToolCallStart {
                    id: "call_main".to_string(),
                    name: "delegate".to_string(),
                },
                Chunk::ToolCallEnd {
                    id: "call_main".to_string(),
                    name: "delegate".to_string(),
                    arguments: r#"{"agent":"coder","goal":"code it"}"#.to_string(),
                },
                Chunk::Done,
            ],
            vec![
                Chunk::TextDelta("main done".to_string()),
                Chunk::Usage(Usage::default()),
                Chunk::Done,
            ],
        ]));
        let mut main_agent = Agent::new(main_p.clone(), 4).with_system_prompt("main");
        main_agent.register_tool(
            Arc::new(RecursiveDelegateTool::new(3).with_sink(engine.clone())) as Arc<dyn Tool>,
        );

        let mut stream = main_agent
            .run_stream(RunInput {
                prompt: "go".to_string(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        let mut text = String::new();
        let mut main_result = String::new();
        while let Some(ev) = stream.next().await {
            match ev.unwrap() {
                RunEvent::TextDelta(t) => text.push_str(&t),
                RunEvent::ToolResult { result, .. } => main_result = result,
                _ => {}
            }
        }

        assert!(text.contains("main done"), "got: {text}");
        assert!(
            main_result.contains("[delegate:coder]"),
            "got: {main_result}"
        );
        assert!(main_result.contains("coder finished"), "got: {main_result}");
        assert!(coder_p.call_count() >= 2, "coder 两轮：工具轮 + 收尾轮");
        assert!(
            leaf_p.call_count() >= 1,
            "leaf 子代理必须被递归调用（深度 3）"
        );
    }

    #[tokio::test]
    async fn end_to_end_engine_depth_limit_blocks_nested() {
        use crate::agent::Agent;
        use crate::delegate::DelegateEngine;
        use crate::test_utils::MockProvider;
        use deepseeknova_core::chunk::{Chunk, Usage};
        use deepseeknova_core::{RunEvent, RunInput, Runner};
        use std::collections::HashMap;
        use tokio_stream::StreamExt;

        let leaf_p = Arc::new(MockProvider::text("leaf output"));
        let coder_p = Arc::new(MockProvider::sequential(vec![
            vec![
                Chunk::ToolCallStart {
                    id: "call_nested".to_string(),
                    name: "delegate".to_string(),
                },
                Chunk::ToolCallEnd {
                    id: "call_nested".to_string(),
                    name: "delegate".to_string(),
                    arguments: r#"{"agent":"leaf","goal":"leaf work"}"#.to_string(),
                },
                Chunk::Done,
            ],
            vec![
                Chunk::TextDelta("coder finished".to_string()),
                Chunk::Usage(Usage::default()),
                Chunk::Done,
            ],
        ]));

        let sink_slot: Arc<std::sync::OnceLock<Arc<dyn DelegationSink>>> =
            Arc::new(std::sync::OnceLock::new());
        let late = Arc::new(LateSink(sink_slot.clone()));

        let mut coder = Agent::new(coder_p.clone(), 4)
            .with_system_prompt("coder")
            .with_extension(DelegateDepth(2));
        coder.register_tool(
            Arc::new(RecursiveDelegateTool::new(3).with_sink(late)) as Arc<dyn Tool>,
        );
        let leaf = Agent::new(leaf_p.clone(), 3).with_system_prompt("leaf");

        let mut agents: HashMap<String, Arc<Agent>> = HashMap::new();
        agents.insert("coder".into(), Arc::new(coder));
        agents.insert("leaf".into(), Arc::new(leaf));
        // max_depth=2：主 agent 派发 coder（depth 2）放行，coder 再派 leaf
        // （depth 3）被引擎守门拒绝 → 优雅降级，不硬失败。
        let engine = Arc::new(DelegateEngine::new(agents, 2, 2000).with_max_depth(2));
        let _ = sink_slot.set(engine.clone() as Arc<dyn DelegationSink>);

        // 引擎守门直接验证：depth 3 > max 2 → 拒绝（清晰错误文本）。
        let err = engine
            .run_at_depth("leaf", "x", InputValues::new(), 3)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("recursion depth exceeded (max 2"),
            "got: {err}"
        );

        let main_p = Arc::new(MockProvider::sequential(vec![
            vec![
                Chunk::ToolCallStart {
                    id: "call_main".to_string(),
                    name: "delegate".to_string(),
                },
                Chunk::ToolCallEnd {
                    id: "call_main".to_string(),
                    name: "delegate".to_string(),
                    arguments: r#"{"agent":"coder","goal":"code it"}"#.to_string(),
                },
                Chunk::Done,
            ],
            vec![
                Chunk::TextDelta("main done".to_string()),
                Chunk::Usage(Usage::default()),
                Chunk::Done,
            ],
        ]));
        let mut main_agent = Agent::new(main_p.clone(), 4).with_system_prompt("main");
        main_agent.register_tool(
            Arc::new(RecursiveDelegateTool::new(3).with_sink(engine.clone())) as Arc<dyn Tool>,
        );

        let mut stream = main_agent
            .run_stream(RunInput {
                prompt: "go".to_string(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        let mut text = String::new();
        while let Some(ev) = stream.next().await {
            if let Ok(RunEvent::TextDelta(t)) = ev {
                text.push_str(&t);
            }
        }

        assert!(text.contains("main done"), "got: {text}");
        assert_eq!(
            coder_p.call_count(),
            2,
            "coder 两轮正常完成（嵌套被优雅拒绝，不硬失败）"
        );
        assert_eq!(leaf_p.call_count(), 0, "超深时 leaf 不得被调用");
    }
}
