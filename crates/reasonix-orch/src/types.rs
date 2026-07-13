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
    /// Dependency edges: action_id → [dependency_action_ids].
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
    pub provider: std::sync::Arc<dyn reasonix_core::Runner + Send>,
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
