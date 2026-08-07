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

use crate::task_spec::InputValues;
use async_trait::async_trait;
use deepseeknova_core::Tool;
use deepseeknova_core::{ToolContext, ToolSchema};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

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
    ) -> anyhow::Result<String>;
}

/// 当前子代理调用深度的执行期扩展。由子代理循环注入每个 ToolContext；
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

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        if ctx.cancellation.is_cancelled() {
            anyhow::bail!("cancelled");
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
        match sink.delegate(&parsed.agent, &goal, &values, next).await {
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
        ) -> anyhow::Result<String> {
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
                anyhow::bail!("boom at depth {depth} for {agent}");
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
}
