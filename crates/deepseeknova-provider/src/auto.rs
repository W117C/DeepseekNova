//! Auto model + thinking routing.
//!
//! [`ModelAutoRouter`] implements [`AutoRouteDecider`]: the agent loop calls it
//! once per `run_stream` (not once per step), so concurrent runs never share
//! routing state. A cheap model decides whether the turn should run on the
//! small/fast model with thinking off, or the large model with thinking on.
//!
//! Any router failure returns `None`, which keeps the caller's default
//! provider — routing must never make the real turn worse.

use crate::cost::ModelRole;
use crate::factory::ReasoningEffort;
use crate::router::ModelRouter;
use crate::{Provider, ValidatedRequest};
use async_trait::async_trait;
use deepseeknova_core::{Message, Role};
use std::sync::Arc;

const ROUTE_SYSTEM_PROMPT: &str = r#"You are a routing classifier for an AI coding agent.
Decide how much model power the current user turn needs, then answer with a
single JSON object, no prose:
{"model":"flash"|"pro","thinking":"off"|"high"|"max"}

Rules:
- Short, mechanical, simple questions -> flash + off.
- Coding, debugging, architecture, security review, refactoring, migration,
  ambiguous multi-step tasks, or anything where a wrong answer is expensive -> pro + high.
- Long or deeply uncertain work may use pro + max, but prefer high unless the
  task is genuinely hard.
Never output anything except the JSON object."#;

const HEURISTIC_LONG_CHARS: usize = 2000;
const HEURISTIC_KEYWORDS: &[&str] = &[
    "debug",
    "security",
    "review",
    "refactor",
    "architect",
    "migrat",
    "bug",
    "crash",
    "deadlock",
    "performance",
    "并发",
    "安全",
    "架构",
    "重构",
    "调试",
];

/// Which route to take for the real call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteChoice {
    /// Leave the decision to the wrapped provider (fallback).
    Auto,
    /// Small/fast model with the given reasoning effort.
    Flash(ReasoningEffort),
    /// Large model with the given reasoning effort.
    Pro(ReasoningEffort),
}

/// Per-run auto routing: decide once for a conversation and resolve it to a
/// concrete provider. Returning `None` means "keep the caller's default
/// provider" (router failure, no matching pointer, or explicit override).
#[async_trait]
pub trait AutoRouteDecider: Send + Sync {
    async fn decide(&self, messages: &[Message]) -> Option<Arc<dyn Provider>>;
}

/// Concrete router backed by [`ModelRouter`].
pub struct ModelAutoRouter {
    router: Arc<ModelRouter>,
    routing_model: Option<String>,
    max_chars: usize,
}

impl ModelAutoRouter {
    pub fn new(router: Arc<ModelRouter>, routing_model: Option<String>, max_chars: usize) -> Self {
        Self {
            router,
            routing_model,
            max_chars: max_chars.max(512),
        }
    }
}

impl ModelAutoRouter {
    async fn decide_choice(
        &self,
        messages: &[Message],
    ) -> Result<RouteChoice, deepseeknova_core::DeepseeknovaError> {
        let prompt = latest_user_text(messages, self.max_chars).ok_or_else(|| {
            deepseeknova_core::DeepseeknovaError::config(
                "no user message available for routing".to_string(),
            )
        })?;
        let sys = Message {
            role: Role::System,
            content: ROUTE_SYSTEM_PROMPT.to_string(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        };
        let user = Message {
            role: Role::User,
            content: prompt.clone(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        };
        let provider = self.router.provider_for_maybe_model(
            ModelRole::Quick,
            self.routing_model.as_deref(),
            Some(ReasoningEffort::Disabled),
        )?;
        let routing_messages = [sys, user];
        let validated = ValidatedRequest::new(&routing_messages, &[]).map_err(|e| {
            deepseeknova_core::DeepseeknovaError::provider(format!(
                "routing messages failed replay invariant: {e:?}"
            ))
        })?;
        let reply = provider.generate(validated).await?;
        parse_choice(&reply.content)
            .or_else(|| heuristic_choice(&prompt))
            .ok_or_else(|| {
                deepseeknova_core::DeepseeknovaError::provider(format!(
                    "unable to parse route decision: {}",
                    reply.content
                ))
            })
    }

    fn resolve(
        &self,
        choice: &RouteChoice,
    ) -> Result<Option<Arc<dyn Provider>>, deepseeknova_core::DeepseeknovaError> {
        let (model, effort) = match choice {
            RouteChoice::Auto => return Ok(None),
            RouteChoice::Flash(effort) => {
                let m = self
                    .router
                    .pointer(ModelRole::Quick)
                    .or_else(|| self.router.pointer(ModelRole::Main));
                (m, effort)
            }
            RouteChoice::Pro(effort) => (self.router.pointer(ModelRole::Main), effort),
        };
        match model {
            Some(model) => Ok(Some(self.router.provider_for_model(
                &model,
                ModelRole::Main,
                Some(*effort),
            )?)),
            None => Ok(None),
        }
    }
}

#[async_trait]
impl AutoRouteDecider for ModelAutoRouter {
    async fn decide(&self, messages: &[Message]) -> Option<Arc<dyn Provider>> {
        let choice = match self.decide_choice(messages).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("auto route decision failed, using fallback: {e}");
                RouteChoice::Auto
            }
        };
        self.resolve(&choice).ok().flatten()
    }
}

fn latest_user_text(messages: &[Message], max_chars: usize) -> Option<String> {
    let text = messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .map(|m| m.content.clone())?;
    let cut: String = text.chars().take(max_chars).collect();
    Some(if cut.chars().count() < text.chars().count() {
        format!("{cut}\n[truncated]")
    } else {
        cut
    })
}

fn parse_choice(text: &str) -> Option<RouteChoice> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(&text[start..=end]).ok()?;
    let model = value.get("model")?.as_str()?;
    let thinking = value
        .get("thinking")
        .and_then(|v| v.as_str())
        .unwrap_or("off");
    let effort = match thinking {
        "high" => ReasoningEffort::High,
        "max" => ReasoningEffort::Max,
        _ => ReasoningEffort::Disabled,
    };
    match model {
        "flash" => Some(RouteChoice::Flash(effort)),
        "pro" => Some(RouteChoice::Pro(effort)),
        "auto" => Some(RouteChoice::Auto),
        _ => None,
    }
}

fn heuristic_choice(prompt: &str) -> Option<RouteChoice> {
    let lower = prompt.to_ascii_lowercase();
    let complex = prompt.chars().count() > HEURISTIC_LONG_CHARS
        || HEURISTIC_KEYWORDS
            .iter()
            .any(|k| lower.contains(&k.to_ascii_lowercase()));
    Some(if complex {
        RouteChoice::Pro(ReasoningEffort::High)
    } else {
        RouteChoice::Flash(ReasoningEffort::Disabled)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cost::CostLedger;

    fn user_msg(content: &str) -> Message {
        Message {
            role: Role::User,
            content: content.to_string(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    #[test]
    fn parses_route_json() {
        assert_eq!(
            parse_choice("Sure!\n{\"model\":\"flash\",\"thinking\":\"off\"}"),
            Some(RouteChoice::Flash(ReasoningEffort::Disabled))
        );
        assert_eq!(
            parse_choice("{\"model\":\"pro\",\"thinking\":\"high\"}"),
            Some(RouteChoice::Pro(ReasoningEffort::High))
        );
        assert_eq!(
            parse_choice("{\"model\":\"pro\",\"thinking\":\"max\"}"),
            Some(RouteChoice::Pro(ReasoningEffort::Max))
        );
        assert_eq!(parse_choice("no json"), None);
    }

    #[test]
    fn heuristic_marks_complex_tasks() {
        assert_eq!(
            heuristic_choice("fix this typo"),
            Some(RouteChoice::Flash(ReasoningEffort::Disabled))
        );
        assert_eq!(
            heuristic_choice("debug this security issue"),
            Some(RouteChoice::Pro(ReasoningEffort::High))
        );
        let long = "x".repeat(3000);
        assert_eq!(
            heuristic_choice(&long),
            Some(RouteChoice::Pro(ReasoningEffort::High))
        );
    }

    #[test]
    fn latest_user_truncates_long_messages() {
        let msgs = vec![user_msg(&"a".repeat(1000))];
        let text = latest_user_text(&msgs, 100).unwrap();
        assert!(text.contains("[truncated]"));
        assert!(text.chars().count() <= 120);
    }

    #[test]
    fn router_builds_from_config_with_pointers() {
        std::env::set_var("DPNOVA_AUTO_TEST_KEY", "test");
        let cfg: deepseeknova_config::Config = toml::from_str(
            r#"
            [[providers]]
            name = "deepseek"
            kind = "openai"
            api_key_env = "DPNOVA_AUTO_TEST_KEY"

            [[models]]
            name = "big"
            provider = "deepseek"

            [[models]]
            name = "small"
            provider = "deepseek"

            [model_pointers]
            main = "big"
            quick = "small"
        "#,
        )
        .unwrap();
        let router = ModelRouter::from_config(&cfg, Arc::new(CostLedger::new())).unwrap();
        let auto = ModelAutoRouter::new(Arc::new(router), None, 1000);
        assert_eq!(
            auto.router.pointer(ModelRole::Quick).as_deref(),
            Some("small")
        );
        assert_eq!(auto.router.pointer(ModelRole::Main).as_deref(), Some("big"));
    }
}
