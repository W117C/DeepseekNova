//! # DelegateEngine — 模型自主 spawn 子代理（Claude Code Task-tool 式）
//!
//! 子代理是受限工具集的 [`Agent`] 实例：独立上下文、真正执行工具、只回传封顶摘要、
//! 工具集不含 `delegate`（禁递归）。并发受信号量限制，满员时排队等待。

use crate::agent::Agent;
use deepseeknova_core::{RunEvent, RunInput, Runner};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio_stream::StreamExt;

/// 一个内置子代理预设。`tools` 为工具 schema 名白名单（均不含 "delegate"）。
#[derive(Debug, Clone)]
pub struct DelegatePreset {
    pub name: String,
    pub system_prompt: String,
    pub tools: Vec<String>,
    pub max_steps: usize,
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// 4 个内置预设。工具名对应真实 schema 名（read_file/bash/search_code…）；均不含 delegate。
pub fn builtin_presets() -> Vec<DelegatePreset> {
    vec![
        DelegatePreset {
            name: "explorer".into(),
            system_prompt: "You are an explorer sub-agent. Investigate and locate relevant code/facts read-only. \
                Prefer graph tools (search_code/traverse_graph/retrieve_entity) over full-file reads. \
                Return a concise findings summary.".into(),
            tools: names(&["read_file", "ls", "glob", "grep", "search_code", "traverse_graph", "retrieve_entity", "recall", "web_fetch"]),
            max_steps: 10,
        },
        DelegatePreset {
            name: "coder".into(),
            system_prompt: "You are a coder sub-agent. Implement the requested change: read, edit/write files, \
                run shell as needed. Return a concise summary of what changed.".into(),
            tools: names(&["read_file", "write_file", "edit_file", "move_file", "ls", "glob", "grep", "bash", "search_code", "traverse_graph", "retrieve_entity"]),
            max_steps: 15,
        },
        DelegatePreset {
            name: "tester".into(),
            system_prompt: "You are a tester sub-agent. Run tests / reproduce issues via shell and report results \
                concisely. Do not modify source files.".into(),
            tools: names(&["read_file", "ls", "glob", "grep", "bash"]),
            max_steps: 10,
        },
        DelegatePreset {
            name: "reviewer".into(),
            system_prompt: "You are a reviewer sub-agent. Review code read-only and report issues concisely. \
                Do not modify files.".into(),
            tools: names(&["read_file", "ls", "glob", "grep", "search_code", "traverse_graph", "retrieve_entity"]),
            max_steps: 10,
        },
    ]
}

/// 委派引擎：持有每个预设一个配置好的 [`Agent`]，并发受信号量限制。
pub struct DelegateEngine {
    agents: HashMap<String, Arc<Agent>>,
    semaphore: Arc<Semaphore>,
    output_cap_tokens: usize,
}

impl DelegateEngine {
    pub fn new(
        agents: HashMap<String, Arc<Agent>>,
        max_concurrent: usize,
        output_cap_tokens: usize,
    ) -> Self {
        Self {
            agents,
            semaphore: Arc::new(Semaphore::new(max_concurrent.max(1))),
            output_cap_tokens,
        }
    }

    /// 已注册的子代理名（供工具做友好错误提示）。
    pub fn agent_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.agents.keys().cloned().collect();
        v.sort();
        v
    }

    /// 委派一个子代理执行 goal，返回封顶后的结果摘要。
    /// 信号量满时 **排队等待**（不拒绝）。
    pub async fn run(&self, agent: &str, goal: &str) -> anyhow::Result<String> {
        let sub = self
            .agents
            .get(agent)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown sub-agent '{agent}'"))?;

        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|_| anyhow::anyhow!("delegate semaphore closed"))?;

        let input = RunInput {
            prompt: goal.to_string(),
            images: vec![],
            model_override: None,
        };
        let text = collect_final_text(sub.as_ref(), input).await?;
        Ok(cap_output(&text, self.output_cap_tokens))
    }
}

/// 驱动子 Agent 的 run_stream 并收集最终文本（与 CLI/desktop 收集方式一致）。
async fn collect_final_text(agent: &Agent, input: RunInput) -> anyhow::Result<String> {
    let mut stream = agent.run_stream(input).await?;
    let mut final_text = String::new();
    while let Some(ev) = stream.next().await {
        match ev? {
            RunEvent::TextDelta(t) => final_text.push_str(&t),
            RunEvent::Done(out) if !out.text.is_empty() => {
                final_text = out.text;
            }
            _ => {}
        }
    }
    Ok(final_text)
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

    #[test]
    fn presets_never_include_delegate_tool() {
        // 禁递归的可测形式：任何预设的工具集都不含 "delegate"。
        for p in builtin_presets() {
            assert!(
                !p.tools.iter().any(|t| t == "delegate"),
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
}
