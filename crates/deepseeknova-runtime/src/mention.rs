//! 主对话 @-mention 派发包装器。
//!
//! 主 Agent 与 [`SubAgentRunner`] 之间的运行时选择器：prompt 含**已知**
//! `@子代理` 引用 → 交给 `SubAgentRunner`（自带 mention 解析/消歧语义）；
//! 零引用 → 原样走主 Agent。歧义引用直接上抛（不静默降级到主 Agent，
//! 避免用户以为子代理执行了但实际由主 Agent 代答）。

use std::sync::Arc;

use async_trait::async_trait;
use deepseeknova_agent::mention::resolve_mention;
use deepseeknova_agent::SubAgentRunner;
use deepseeknova_core::runner::{RunEventStream, RunInput, Runner};
use deepseeknova_core::DeepseeknovaError;

/// 主对话入口选择器：`@name` 已知 → 子代理，否则主 Agent。
pub struct MentionAwareRunner {
    main: Arc<dyn Runner>,
    sub: Arc<SubAgentRunner>,
}

impl MentionAwareRunner {
    /// 包一层主 Agent 与已装配的子代理 runner。
    pub fn new(main: Arc<dyn Runner>, sub: SubAgentRunner) -> Self {
        Self {
            main,
            sub: Arc::new(sub),
        }
    }

    /// 已注册子代理名（供上层做 @-mention 补全等展示）。
    pub fn agent_names(&self) -> Vec<String> {
        self.sub.agent_names()
    }
}

#[async_trait]
impl Runner for MentionAwareRunner {
    async fn run_stream(&self, input: RunInput) -> Result<RunEventStream, DeepseeknovaError> {
        let known = self.sub.agent_names();
        match resolve_mention(&input.prompt, &known)? {
            Some(_) => self.sub.run_stream(input).await,
            None => self.main.run_stream(input).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_agent::test_utils::MockRunner;
    use deepseeknova_agent::SubAgentConfig;
    use futures::StreamExt;

    fn sub_runner_with(name: &str) -> SubAgentRunner {
        let mut sub = SubAgentRunner::new(Arc::new(
            deepseeknova_agent::test_utils::MockProvider::text("sub-agent reply"),
        ));
        sub.register(SubAgentConfig::new(name, "you are a sub agent"));
        sub
    }

    async fn collect_text(runner: &dyn Runner, prompt: &str) -> String {
        let mut stream = runner
            .run_stream(RunInput {
                prompt: prompt.to_string(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        let mut out = String::new();
        while let Some(ev) = stream.next().await {
            match ev.unwrap() {
                deepseeknova_core::runner::RunEvent::TextDelta(t) => out.push_str(&t),
                deepseeknova_core::runner::RunEvent::Done(r) => out = r.text.clone(),
                _ => {}
            }
        }
        out
    }

    #[tokio::test]
    async fn known_mention_routes_to_sub_agent() {
        let main = Arc::new(MockRunner::text("main-agent reply"));
        let runner = MentionAwareRunner::new(main, sub_runner_with("coder"));
        let out = collect_text(&runner, "@coder fix the bug").await;
        assert_eq!(out, "sub-agent reply");
    }

    #[tokio::test]
    async fn plain_prompt_keeps_main_agent() {
        let main = Arc::new(MockRunner::text("main-agent reply"));
        let runner = MentionAwareRunner::new(main, sub_runner_with("coder"));
        let out = collect_text(&runner, "just talk").await;
        assert_eq!(out, "main-agent reply");
    }

    #[tokio::test]
    async fn ambiguous_mention_errors_instead_of_falling_back() {
        let main = Arc::new(MockRunner::text("main-agent reply"));
        let mut sub = sub_runner_with("coder");
        sub.register(SubAgentConfig::new("reviewer", "review"));
        let runner = MentionAwareRunner::new(main, sub);
        let result = runner
            .run_stream(RunInput {
                prompt: "@coder and @reviewer both".into(),
                images: vec![],
                model_override: None,
            })
            .await;
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("歧义 @-mention 必须报错，不得静默回退主 Agent"),
        };
        assert!(
            err.to_string().contains("ambiguous @-mention"),
            "got: {err}"
        );
    }
}
