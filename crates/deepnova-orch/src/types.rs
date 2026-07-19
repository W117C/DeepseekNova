//! Core types for the orchestration system.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Goal Planner types
// ---------------------------------------------------------------------------

/// A high-level goal expressed in natural language.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Goal {
    /// Natural language description of the goal.
    pub description: String,
    /// Optional constraints (e.g., "use Rust", "must be tested").
    pub constraints: Vec<String>,
    /// Success criteria.
    pub criteria: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Goal ─────────────────────────────────────────────────────

    #[test]
    fn test_goal_round_trip_serde() {
        let goal = Goal {
            description: "Build a CLI tool in Rust".into(),
            constraints: vec!["use clap".into(), "async".into()],
            criteria: vec!["compiles".into(), "tests pass".into()],
        };
        let json = serde_json::to_string(&goal).unwrap();
        let deserialized: Goal = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.description, goal.description);
        assert_eq!(deserialized.constraints, goal.constraints);
    }

    // ── Action + ActionStatus ────────────────────────────────────

    #[test]
    fn test_action_status_equality() {
        assert_eq!(ActionStatus::Pending, ActionStatus::Pending);
        assert_eq!(ActionStatus::Completed, ActionStatus::Completed);
        assert_ne!(ActionStatus::Pending, ActionStatus::Completed);
        assert!(matches!(
            ActionStatus::Failed("err".into()),
            ActionStatus::Failed(_)
        ));
    }

    #[test]
    fn test_action_defaults_to_pending() {
        let action = Action {
            id: "act-1".into(),
            name: "create_file".into(),
            description: "Create main.rs".into(),
            preconditions: vec![],
            effects: vec!["file exists".into()],
            cost: 1.0,
            tool: Some("write_file".into()),
            tool_args: None,
            delegatable: false,
            status: ActionStatus::Pending,
        };
        assert_eq!(action.status, ActionStatus::Pending);
    }

    // ── Plan + PlanStatus ────────────────────────────────────────

    #[test]
    fn test_plan_status_transitions() {
        // Verify all variants are constructable and partial eq works
        assert_ne!(PlanStatus::Draft, PlanStatus::Completed);
    }

    #[test]
    fn test_plan_with_dependencies() {
        let goal = Goal {
            description: "Test plan".into(),
            constraints: vec![],
            criteria: vec![],
        };
        let mut dependencies = std::collections::HashMap::new();
        dependencies.insert("act-2".into(), vec!["act-1".into()]);
        dependencies.insert("act-3".into(), vec!["act-1".into(), "act-2".into()]);

        let plan = Plan {
            id: "plan-1".into(),
            goal,
            actions: vec![],
            dependencies,
            status: PlanStatus::Draft,
            reasoning: None,
            usage: None,
        };

        // act-3 depends on act-1 and act-2
        let deps = plan.dependencies.get("act-3").unwrap();
        assert_eq!(deps.len(), 2);
        assert!(deps.contains(&"act-1".to_string()));
    }

    #[test]
    fn test_plan_serde_round_trip() {
        let plan = Plan {
            id: "plan-1".into(),
            goal: Goal {
                description: "goal".into(),
                constraints: vec![],
                criteria: vec![],
            },
            actions: vec![Action {
                id: "a1".into(),
                name: "action-1".into(),
                description: "desc".into(),
                preconditions: vec!["cond".into()],
                effects: vec!["done".into()],
                cost: 1.0,
                tool: None,
                tool_args: None,
                delegatable: false,
                status: ActionStatus::Pending,
            }],
            dependencies: std::collections::HashMap::new(),
            status: PlanStatus::Draft,
            reasoning: Some("thinking...".into()),
            usage: Some(PlanUsage {
                prompt_tokens: 100,
                completion_tokens: 50,
                cache_hit_tokens: 20,
                cache_miss_tokens: 80,
            }),
        };
        let json = serde_json::to_string_pretty(&plan).unwrap();
        let deserialized: Plan = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, "plan-1");
        assert_eq!(deserialized.actions.len(), 1);
        assert_eq!(deserialized.usage.unwrap().prompt_tokens, 100);
    }

    // ── Swarm types ─────────────────────────────────────────────

    #[test]
    fn test_agent_role_variants() {
        assert_ne!(AgentRole::Queen, AgentRole::Worker);
        assert_eq!(AgentRole::Worker, AgentRole::Worker);
    }

    #[test]
    fn test_swarm_task_status() {
        assert_eq!(SwarmTaskStatus::Pending, SwarmTaskStatus::Pending);
        assert_eq!(
            SwarmTaskStatus::Failed("x".into()),
            SwarmTaskStatus::Failed("x".into())
        );
        assert_ne!(
            SwarmTaskStatus::Failed("a".into()),
            SwarmTaskStatus::Failed("b".into())
        );
    }

    #[test]
    fn test_swarm_message_type() {
        assert_eq!(
            SwarmMessageType::TaskAssignment,
            SwarmMessageType::TaskAssignment
        );
        assert_ne!(
            SwarmMessageType::TaskAssignment,
            SwarmMessageType::Coordination
        );
    }

    #[test]
    fn test_swarm_task_serde_round_trip() {
        let task = SwarmTask {
            id: "task-1".into(),
            description: "Write unit tests".into(),
            assigned_to: "worker-1".into(),
            action_id: "act-1".into(),
            status: SwarmTaskStatus::InProgress,
            output: None,
            reasoning: None,
        };
        let json = serde_json::to_string(&task).unwrap();
        let deserialized: SwarmTask = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, "task-1");
        assert_eq!(deserialized.status, SwarmTaskStatus::InProgress);
    }
}

/// A single action in the GOAP plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    /// Unique action identifier.
    pub id: String,
    /// Action name (e.g., "create_file", "run_test").
    pub name: String,
    /// Description of what this action does.
    pub description: String,
    /// Preconditions — world state must have these true before action.
    pub preconditions: Vec<String>,
    /// Effects — world state changes after action completes.
    pub effects: Vec<String>,
    /// Estimated cost (lower = higher priority).
    pub cost: f32,
    /// Which tool to use (e.g., "edit_file", "bash").
    pub tool: Option<String>,
    /// Tool arguments template.
    pub tool_args: Option<serde_json::Value>,
    /// Whether this action can be delegated to a sub-agent.
    pub delegatable: bool,
    /// Action status.
    pub status: ActionStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ActionStatus {
    Pending,
    InProgress,
    Completed,
    Failed(String),
    Blocked(String),
}

/// The GOAP plan — a DAG of actions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    /// Plan identifier.
    pub id: String,
    /// The original goal.
    pub goal: Goal,
    /// Actions in the plan.
    pub actions: Vec<Action>,
    /// Dependency edges: action_id → \[dependency_action_ids\].
    pub dependencies: HashMap<String, Vec<String>>,
    /// Overall plan status.
    pub status: PlanStatus,
    /// DeepSeek reasoning used during planning.
    pub reasoning: Option<String>,
    /// Token usage for the planning phase.
    pub usage: Option<PlanUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PlanStatus {
    Draft,
    InProgress,
    Completed,
    Failed(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cache_hit_tokens: u32,
    pub cache_miss_tokens: u32,
}

// ---------------------------------------------------------------------------
// Swarm types
// ---------------------------------------------------------------------------

/// Role of an agent in the swarm.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AgentRole {
    /// Queen — coordinates and assigns work.
    Queen,
    /// Worker — executes assigned tasks.
    Worker,
    /// Reviewer — validates work products.
    Reviewer,
    /// Researcher — gathers information.
    Researcher,
}

/// A registered agent in the swarm.
#[derive(Clone)]
pub struct SwarmAgent {
    pub id: String,
    pub name: String,
    pub role: AgentRole,
    pub provider: std::sync::Arc<dyn deepnova_core::Runner + Send>,
    pub system_prompt: String,
    pub max_steps: usize,
}

/// A task assigned to a swarm agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmTask {
    pub id: String,
    pub description: String,
    pub assigned_to: String,
    pub action_id: String,
    pub status: SwarmTaskStatus,
    pub output: Option<String>,
    pub reasoning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SwarmTaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed(String),
}

/// Message exchanged between swarm agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmMessage {
    pub from: String,
    pub to: String,
    pub content: String,
    pub message_type: SwarmMessageType,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SwarmMessageType {
    TaskAssignment,
    TaskResult,
    StatusUpdate,
    Question,
    Coordination,
}
