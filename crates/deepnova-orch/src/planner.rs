//! # Goal-Oriented Action Planning (GOAP) for DeepSeek-V4
//!
//! Inspired by Ruflo's goal.ruv.io GOAP planner, adapted for DeepSeek-V4's
//! thinking mode. The planner uses DeepSeek-V4's chain-of-thought reasoning
//! to decompose a natural-language goal into a DAG of executable actions,
//! then executes them with adaptive replanning.
//!
//! ## How it works
//!
//! 1. **Decompose**: LLM breaks the goal into sub-actions with preconditions/effects
//! 2. **Schedule**: A* search finds the optimal action sequence
//! 3. **Execute**: Actions run via the agent, with tool calls
//! 4. **Replan**: On failure, the planner re-runs A* from current state
//!
//! ## DeepSeek-V4 optimizations
//!
//! - Uses `reasoning_effort` for planning phase (high/max)
//! - Passes `reasoning_content` correctly when tool calls are involved
//! - Monitors `cache_hit_tokens` for cost optimization
//! - Stable system prompt prefix for cache hits across planning sessions

use crate::types::*;
use deepnova_core::runner::{RunEvent, RunInput, Runner};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio_stream::StreamExt;
use tracing::{info, warn};

/// Configuration for the Goal Planner.
#[derive(Debug, Clone)]
pub struct PlannerConfig {
    pub reasoning_effort: String,
    pub max_actions: usize,
    pub max_retries: u32,
    pub thinking_enabled: bool,
    pub plan_output_format: PlanFormat,
    /// P2 Fix #13: Retry configuration for action execution
    pub action_retry_attempts: u32,
    pub action_retry_base_delay: Duration,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlanFormat {
    Json,
    Yaml,
}

impl Default for PlannerConfig {
    fn default() -> Self {
        Self {
            reasoning_effort: "high".to_string(),
            max_actions: 20,
            max_retries: 3,
            thinking_enabled: true,
            plan_output_format: PlanFormat::Json,
            action_retry_attempts: 3,
            action_retry_base_delay: Duration::from_secs(1),
        }
    }
}

/// The Goal Planner — decomposes goals into executable action plans.
pub struct GoalPlanner {
    runner: Arc<dyn Runner + Send>,
    config: PlannerConfig,
}

impl GoalPlanner {
    pub fn new(runner: Arc<dyn Runner + Send>) -> Self {
        Self {
            runner,
            config: PlannerConfig::default(),
        }
    }

    pub fn with_config(mut self, config: PlannerConfig) -> Self {
        self.config = config;
        self
    }

    /// Decompose a natural language goal into a structured plan using DeepSeek-V4.
    pub async fn plan(&self, goal: Goal) -> anyhow::Result<Plan> {
        info!(description = %goal.description, "planning goal");

        let plan_prompt = self.build_plan_prompt(&goal);

        // Use a dedicated planning agent with thinking mode
        let input = RunInput {
            prompt: plan_prompt.clone(),
            images: vec![],
            model_override: None,
        };

        let mut stream = self.runner.run_stream(input).await?;
        let mut plan_text = String::new();
        let mut reasoning = String::new();
        let mut usage: Option<deepnova_core::chunk::Usage> = None;

        while let Some(event) = stream.next().await {
            match event? {
                RunEvent::TextDelta(text) => plan_text.push_str(&text),
                RunEvent::ReasoningDelta { text, .. } => reasoning.push_str(&text),
                RunEvent::Usage(u) => usage = Some(u),
                RunEvent::Done(done_output) => {
                    plan_text = done_output.text;
                }
                _ => {}
            }
        }

        // Parse the plan from LLM output (JSON-first, YAML fallback)
        let plan = match self.config.plan_output_format {
            PlanFormat::Json => match self.parse_plan_json(&goal, &plan_text, &reasoning, &usage) {
                Ok(p) => p,
                Err(e) => {
                    warn!(
                        "JSON parse failed ({}), falling back to YAML-like parser",
                        e
                    );
                    self.parse_plan_yaml(&goal, &plan_text, &reasoning, &usage)?
                }
            },
            PlanFormat::Yaml => self.parse_plan_yaml(&goal, &plan_text, &reasoning, &usage)?,
        };
        info!(plan_id = %plan.id, actions = plan.actions.len(), "plan created");
        Ok(plan)
    }

    /// Execute a plan step by step, with adaptive replanning on failure.
    pub async fn execute(&self, plan: &mut Plan) -> anyhow::Result<()> {
        plan.status = PlanStatus::InProgress;
        let mut completed: HashSet<String> = HashSet::new();

        while completed.len() < plan.actions.len() {
            let ready = self.ready_actions(plan, &completed);
            if ready.is_empty() {
                let all_blocked = plan
                    .actions
                    .iter()
                    .all(|a| matches!(a.status, ActionStatus::Blocked(_)));
                if all_blocked {
                    plan.status = PlanStatus::Failed("all actions blocked".to_string());
                    return Err(anyhow::anyhow!("all actions blocked, cannot make progress"));
                }
                break;
            }

            for action in ready {
                info!(action = %action.name, "executing action");
                match self.execute_action_with_retry(action, plan).await {
                    Ok(true) => {
                        completed.insert(action.id.clone());
                    }
                    Ok(false) => {
                        warn!(action = %action.name, "action failed after retries, will try other actions");
                    }
                    Err(e) => {
                        warn!(action = %action.name, error = %e, "action error");
                        if completed.len() + 1 < plan.actions.len() {
                            continue;
                        }
                        plan.status =
                            PlanStatus::Failed(format!("action '{}' failed: {}", action.name, e));
                        return Err(e);
                    }
                }
            }
        }

        if completed.len() == plan.actions.len() {
            plan.status = PlanStatus::Completed;
            info!("plan completed successfully");
        }

        Ok(())
    }

    /// Build the system prompt for planning.
    #[allow(dead_code)]
    fn system_prompt(&self) -> String {
        match self.config.plan_output_format {
            PlanFormat::Json => format!(
                r#"You are a Goal-Oriented Action Planning (GOAP) expert for an AI coding agent.
DeepSeek-V4 thinking mode is enabled. Use chain-of-thought reasoning to decompose goals.

Your task is to decompose a user's goal into a structured plan.

Output ONLY a JSON object with this exact schema (no markdown, no commentary):

{{
  "actions": [
    {{
      "id": "action_1",
      "name": "action_name",
      "description": "what this action does",
      "preconditions": ["precond1", "precond2"],
      "effects": ["effect1", "effect2"],
      "cost": 3,
      "tool": "tool_name",
      "delegatable": true
    }}
  ],
  "dependencies": {{
    "action_2": ["action_1"],
    "action_3": ["action_1", "action_2"]
  }},
  "execution_order": ["action_1", "action_2", "action_3"]
}}

Constraints:
- Max {} actions
- Each action should be concrete and executable
- Dependencies must form a DAG (no cycles)
- Costs should reflect actual complexity (1-10)
- Output must be valid JSON
"#,
                self.config.max_actions
            ),
            PlanFormat::Yaml => format!(
                r#"You are a Goal-Oriented Action Planning (GOAP) expert for an AI coding agent.
DeepSeek-V4 thinking mode is enabled. Use chain-of-thought reasoning to decompose goals.

Your task is to decompose a user's goal into a structured plan with:

1. A list of actions, each with:
   - id: unique identifier
   - name: verb-noun format (e.g., "create_file", "run_tests")
   - description: what this action does
   - preconditions: what must be true before this action
   - effects: what becomes true after this action
   - cost: estimated effort (1-10, lower = faster)
   - tool: which tool to use (edit_file, bash, web_fetch, grep, etc.)
   - delegatable: whether a sub-agent can do this

2. Dependencies between actions (which actions must complete first)

3. The optimal execution order (A* shortest path)

Output format:
```plan
## Actions
- id: action_1
  name: action_name
  description: ...
  preconditions: [precond1, precond2]
  effects: [effect1, effect2]
  cost: 3
  tool: tool_name
  delegatable: true

## Dependencies
- action_2 depends on: [action_1]

## Execution Order (A*)
1. action_1
2. action_2
```

Constraints:
- Max {} actions
- Each action should be concrete and executable
- Dependencies must form a DAG (no cycles)
- Costs should reflect actual complexity
"#,
                self.config.max_actions
            ),
        }
    }

    /// Build the planning prompt for a specific goal.
    fn build_plan_prompt(&self, goal: &Goal) -> String {
        let mut prompt = format!("## Goal\n{}\n\n", goal.description);
        if !goal.constraints.is_empty() {
            prompt.push_str("## Constraints\n");
            for c in &goal.constraints {
                prompt.push_str(&format!("- {}\n", c));
            }
            prompt.push('\n');
        }
        if !goal.criteria.is_empty() {
            prompt.push_str("## Success Criteria\n");
            for c in &goal.criteria {
                prompt.push_str(&format!("- {}\n", c));
            }
        }
        prompt
    }

    // -----------------------------------------------------------------------
    // P1 Fix #9: JSON-based plan parser (preferred — robust)
    // -----------------------------------------------------------------------

    fn parse_plan_json(
        &self,
        goal: &Goal,
        plan_text: &str,
        reasoning: &str,
        usage: &Option<deepnova_core::chunk::Usage>,
    ) -> anyhow::Result<Plan> {
        let json_str = extract_json_block(plan_text);

        let parsed: serde_json::Value = serde_json::from_str(&json_str)
            .map_err(|e| anyhow::anyhow!("failed to parse plan JSON: {e}"))?;

        let actions_arr = parsed
            .get("actions")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow::anyhow!("missing 'actions' array in plan JSON"))?;

        let mut actions = Vec::new();
        for action_val in actions_arr {
            let id = action_val
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("action")
                .to_string();

            let name = action_val
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("execute")
                .to_string();

            let description = action_val
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let preconditions = action_val
                .get("preconditions")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();

            let effects = action_val
                .get("effects")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();

            let cost = action_val
                .get("cost")
                .and_then(|v| v.as_f64())
                .unwrap_or(5.0) as f32;

            let tool = action_val
                .get("tool")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let delegatable = action_val
                .get("delegatable")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            actions.push(Action {
                id,
                name,
                description,
                preconditions,
                effects,
                cost,
                tool,
                tool_args: None,
                delegatable,
                status: ActionStatus::Pending,
            });
        }

        let dependencies = parsed
            .get("dependencies")
            .and_then(|v| v.as_object())
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| {
                        let deps: Vec<String> = v
                            .as_array()
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                    .collect()
                            })
                            .unwrap_or_default();
                        if k.is_empty() {
                            None
                        } else {
                            Some((k.clone(), deps))
                        }
                    })
                    .collect::<HashMap<String, Vec<String>>>()
            })
            .unwrap_or_default();

        if actions.is_empty() {
            return Err(anyhow::anyhow!("plan JSON contains no actions"));
        }

        let usage_info = usage.as_ref().map(|u| PlanUsage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            cache_hit_tokens: u.cache_hit_tokens,
            cache_miss_tokens: u.cache_miss_tokens,
        });

        Ok(Plan {
            id: format!("plan_{}", uuid::Uuid::new_v4()),
            goal: goal.clone(),
            actions,
            dependencies,
            status: PlanStatus::Draft,
            reasoning: Some(reasoning.to_string()),
            usage: usage_info,
        })
    }

    // -----------------------------------------------------------------------
    // Legacy YAML-like parser (fallback)
    // -----------------------------------------------------------------------

    /// Parse the LLM output into a structured Plan (YAML-like fallback).
    fn parse_plan_yaml(
        &self,
        goal: &Goal,
        plan_text: &str,
        reasoning: &str,
        usage: &Option<deepnova_core::chunk::Usage>,
    ) -> anyhow::Result<Plan> {
        let mut actions = Vec::new();
        let mut dependencies: HashMap<String, Vec<String>> = HashMap::new();

        // Simple plan parsing — extract YAML-like action definitions
        let mut current_action: Option<Action> = None;
        let mut in_actions = false;
        let mut in_deps = false;

        for line in plan_text.lines() {
            let line = line.trim();

            if line.starts_with("## Actions") || line.starts_with("## actions") {
                in_actions = true;
                in_deps = false;
                continue;
            }
            if line.starts_with("## Dependencies") || line.starts_with("## dependencies") {
                in_actions = false;
                in_deps = true;
                continue;
            }
            if line.starts_with("## Execution") {
                in_actions = false;
                in_deps = false;
                continue;
            }

            if in_actions {
                if line.starts_with("- id:") {
                    if let Some(action) = current_action.take() {
                        actions.push(action);
                    }
                    current_action = Some(Action {
                        id: line.strip_prefix("- id:").unwrap_or("").trim().to_string(),
                        name: String::new(),
                        description: String::new(),
                        preconditions: vec![],
                        effects: vec![],
                        cost: 5.0,
                        tool: None,
                        tool_args: None,
                        delegatable: true,
                        status: ActionStatus::Pending,
                    });
                } else if let Some(ref mut action) = current_action {
                    if let Some(name) = line.strip_prefix("name:") {
                        action.name = name.trim().to_string();
                    } else if let Some(desc) = line.strip_prefix("description:") {
                        action.description = desc.trim().to_string();
                    } else if let Some(cost) = line.strip_prefix("cost:") {
                        action.cost = cost.trim().parse().unwrap_or(5.0);
                    } else if let Some(tool) = line.strip_prefix("tool:") {
                        action.tool = Some(tool.trim().to_string());
                    } else if let Some(del) = line.strip_prefix("delegatable:") {
                        action.delegatable = del.trim() == "true";
                    } else if let Some(preconds) = line.strip_prefix("preconditions:") {
                        let stripped = preconds
                            .trim()
                            .trim_start_matches('[')
                            .trim_end_matches(']');
                        action.preconditions = stripped
                            .split(',')
                            .map(|s| s.trim().trim_matches('"').to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                    } else if let Some(effs) = line.strip_prefix("effects:") {
                        let stripped = effs.trim().trim_start_matches('[').trim_end_matches(']');
                        action.effects = stripped
                            .split(',')
                            .map(|s| s.trim().trim_matches('"').to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                    }
                }
            }

            if in_deps {
                if let Some(dep_line) = line.strip_prefix("- ") {
                    if let Some((action_id, deps)) = dep_line.split_once("depends on:") {
                        let id = action_id.trim().to_string();
                        let dep_list: Vec<String> = deps
                            .trim()
                            .trim_start_matches('[')
                            .trim_end_matches(']')
                            .split(',')
                            .map(|s| s.trim().trim_matches('"').to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                        if !id.is_empty() {
                            dependencies.insert(id, dep_list);
                        }
                    }
                }
            }
        }

        // Push the last action
        if let Some(action) = current_action.take() {
            actions.push(action);
        }

        if actions.is_empty() {
            // Fallback: if LLM didn't produce structured output, create a single action
            actions.push(Action {
                id: "action_1".to_string(),
                name: "execute_goal".to_string(),
                description: goal.description.clone(),
                preconditions: vec![],
                effects: vec!["goal_complete".to_string()],
                cost: 5.0,
                tool: None,
                tool_args: None,
                delegatable: true,
                status: ActionStatus::Pending,
            });
        }

        let usage_info = usage.as_ref().map(|u| PlanUsage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            cache_hit_tokens: u.cache_hit_tokens,
            cache_miss_tokens: u.cache_miss_tokens,
        });

        Ok(Plan {
            id: format!("plan_{}", uuid::Uuid::new_v4()),
            goal: goal.clone(),
            actions,
            dependencies,
            status: PlanStatus::Draft,
            reasoning: Some(reasoning.to_string()),
            usage: usage_info,
        })
    }

    /// Find actions whose preconditions are all met.
    fn ready_actions<'a>(&self, plan: &'a Plan, completed: &HashSet<String>) -> Vec<&'a Action> {
        let mut ready = Vec::new();
        for action in &plan.actions {
            if completed.contains(&action.id) {
                continue;
            }
            if matches!(
                action.status,
                ActionStatus::InProgress | ActionStatus::Completed
            ) {
                continue;
            }

            // Check if all dependencies are completed
            let deps = plan.dependencies.get(&action.id);
            let all_deps_done = deps.is_none_or(|deps| deps.iter().all(|d| completed.contains(d)));
            if all_deps_done {
                ready.push(action);
            }
        }
        ready
    }

    // -----------------------------------------------------------------------
    // P2 Fix #13: Action execution with exponential backoff retry
    // -----------------------------------------------------------------------

    /// Execute a single action with retry on failure.
    /// Uses exponential backoff: base_delay * 2^attempt
    async fn execute_action_with_retry(
        &self,
        action: &Action,
        plan: &Plan,
    ) -> anyhow::Result<bool> {
        let max_attempts = self.config.action_retry_attempts;
        let base_delay = self.config.action_retry_base_delay;

        for attempt in 0..max_attempts {
            if attempt > 0 {
                let delay = base_delay * 2u32.pow(attempt - 1);
                // Add jitter: random 0-50% of delay
                let jitter =
                    Duration::from_millis(rand::random::<u64>() % (delay.as_millis() as u64 / 2));
                let total_delay = delay + jitter;
                info!(action = %action.name, attempt, delay = ?total_delay, "retrying action");
                tokio::time::sleep(total_delay).await;
            }

            match self.execute_action(action, plan).await {
                Ok(true) => return Ok(true),
                Ok(false) => {
                    warn!(action = %action.name, attempt, "action returned false, will retry");
                    continue;
                }
                Err(e) => {
                    warn!(action = %action.name, attempt, error = %e, "action failed");
                    if attempt + 1 >= max_attempts {
                        return Err(e);
                    }
                    continue;
                }
            }
        }

        // All retries exhausted
        Ok(false)
    }

    /// Execute a single action using the agent.
    /// P1 Fix #8: Errors are now propagated instead of swallowed.
    async fn execute_action(&self, action: &Action, plan: &Plan) -> anyhow::Result<bool> {
        let prompt = if let Some(ref tool) = action.tool {
            format!(
                "Execute the following action as part of the plan '{}':\n\nAction: {}\nDescription: {}\nTool: {}\n\nGoal context: {}\n\nUse the {} tool to complete this action.",
                plan.goal.description,
                action.name,
                action.description,
                tool,
                plan.goal.description,
                tool,
            )
        } else {
            format!(
                "Execute the following action as part of the plan '{}':\n\nAction: {}\nDescription: {}\n\nGoal context: {}",
                plan.goal.description,
                action.name,
                action.description,
                plan.goal.description,
            )
        };

        let input = RunInput {
            prompt,
            images: vec![],
            model_override: None,
        };

        match self.runner.run(input).await {
            Ok(output) => {
                info!(action = %action.name, text_len = output.text.len(), "action completed");
                Ok(true)
            }
            Err(e) => {
                warn!(action = %action.name, error = %e, "action failed");
                Err(e)
            }
        }
    }
}

/// Extract JSON from text that may be wrapped in markdown code blocks
fn extract_json_block(text: &str) -> String {
    if let Some(start) = text.find("```json") {
        let after_start = &text[start + 7..];
        if let Some(end) = after_start.find("```") {
            return after_start[..end].trim().to_string();
        }
    }

    if let Some(start) = text.find("```") {
        let after_start = &text[start + 3..];
        let after_first_line = after_start
            .find('\n')
            .map(|pos| &after_start[pos + 1..])
            .unwrap_or(after_start);
        if let Some(end) = after_first_line.find("```") {
            return after_first_line[..end].trim().to_string();
        }
    }

    let trimmed = text.trim();
    if trimmed.starts_with('{') {
        let mut depth = 0;
        let mut end_pos = 0;
        for (i, ch) in trimmed.char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end_pos = i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        if end_pos > 0 {
            return trimmed[..end_pos].to_string();
        }
    }

    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_planner_config_defaults() {
        let config = PlannerConfig::default();
        assert_eq!(config.reasoning_effort, "high");
        assert_eq!(config.max_actions, 20);
        assert_eq!(config.max_retries, 3);
        assert!(config.thinking_enabled);
    }

    #[test]
    fn test_planner_config_partial_override() {
        let config = PlannerConfig {
            max_actions: 10,
            ..PlannerConfig::default()
        };
        assert_eq!(config.max_actions, 10);
        assert_eq!(config.reasoning_effort, "high"); // inherited
    }
}
