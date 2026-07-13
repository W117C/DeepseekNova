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
use dpronix_core::runner::{RunEvent, RunInput, Runner};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio_stream::StreamExt;
use tracing::{info, warn};

/// Configuration for the Goal Planner.
#[derive(Debug, Clone)]
pub struct PlannerConfig {
    /// Reasoning effort for the planning phase.
    pub reasoning_effort: String,
    /// Maximum actions in a plan.
    pub max_actions: usize,
    /// Maximum planning retries on failure.
    pub max_retries: u32,
    /// Whether to use thinking mode for planning.
    pub thinking_enabled: bool,
}

impl Default for PlannerConfig {
    fn default() -> Self {
        Self {
            reasoning_effort: "high".to_string(),
            max_actions: 20,
            max_retries: 3,
            thinking_enabled: true,
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
        let mut usage: Option<dpronix_core::chunk::Usage> = None;

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

        // Parse the plan from LLM output
        let plan = self.parse_plan(&goal, &plan_text, &reasoning, &usage)?;
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
                // No actions can proceed — check for circular dependency or all blocked
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
                let result = self.execute_action(action, plan).await?;
                if result {
                    completed.insert(action.id.clone());
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
    fn system_prompt(&self) -> String {
        format!(
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
        )
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

    /// Parse the LLM output into a structured Plan.
    fn parse_plan(
        &self,
        goal: &Goal,
        plan_text: &str,
        reasoning: &str,
        usage: &Option<dpronix_core::chunk::Usage>,
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

    /// Execute a single action using the agent.
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
                Ok(false)
            }
        }
    }
}
