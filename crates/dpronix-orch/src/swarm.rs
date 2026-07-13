//! # Swarm Coordinator — Multi-Agent Orchestration
//!
//! Inspired by Ruflo's swarm architecture. Coordinates multiple agents
//! in a Queen-led hierarchy, optimized for DeepSeek-V4's thinking mode.
//!
//! ## Queen-led Hierarchy
//!
//! ```text
//!       Queen Agent (planning + coordination)
//!      /         |           \
//!  Worker 1   Worker 2   Worker 3
//!   (code)    (test)     (review)
//!      \         |           /
//!       Shared Memory & Results
//! ```

use crate::types::*;
use dpronix_core::runner::RunInput;
use std::collections::HashMap;
use tokio::sync::mpsc;
use tracing::info;

/// Configuration for the swarm.
#[derive(Debug, Clone)]
pub struct SwarmConfig {
    pub max_workers: usize,
    pub consensus_required: bool,
    pub thinking_enabled: bool,
    pub reasoning_effort: String,
}

impl Default for SwarmConfig {
    fn default() -> Self {
        Self {
            max_workers: 5,
            consensus_required: false,
            thinking_enabled: true,
            reasoning_effort: "high".to_string(),
        }
    }
}

/// The Swarm Coordinator — manages a team of agents working on a shared goal.
pub struct SwarmCoordinator {
    agents: HashMap<String, SwarmAgent>,
    config: SwarmConfig,
    task_tx: mpsc::Sender<SwarmMessage>,
    task_rx: mpsc::Receiver<SwarmMessage>,
}

impl SwarmCoordinator {
    /// Create a new swarm with a Queen agent.
    pub fn new(queen: SwarmAgent, config: SwarmConfig) -> Self {
        let (tx, rx) = mpsc::channel(256);
        let mut agents = HashMap::new();
        agents.insert(queen.id.clone(), queen);
        Self {
            agents,
            config,
            task_tx: tx,
            task_rx: rx,
        }
    }

    /// Register a worker agent in the swarm.
    pub fn register_agent(&mut self, agent: SwarmAgent) {
        info!(name = %agent.name, role = ?agent.role, "agent registered");
        self.agents.insert(agent.id.clone(), agent);
    }

    /// Decompose a goal into sub-tasks and assign to workers.
    pub async fn orchestrate(&mut self, plan: &crate::types::Plan) -> anyhow::Result<Vec<SwarmTask>> {
        let _queen = self.agents.values().find(|a| a.role == AgentRole::Queen)
            .ok_or_else(|| anyhow::anyhow!("no queen agent registered"))?;

        info!(plan_id = %plan.id, agents = self.agents.len(), "orchestrating swarm");

        let mut tasks = Vec::new();
        for action in &plan.actions {
            if !action.delegatable {
                continue;
            }

            // Find the most suitable worker for this action
            let worker = self.select_worker(action)?;
            let task = SwarmTask {
                id: format!("task_{}", uuid::Uuid::new_v4()),
                description: action.description.clone(),
                assigned_to: worker.id.clone(),
                action_id: action.id.clone(),
                status: SwarmTaskStatus::Pending,
                output: None,
                reasoning: None,
            };
            tasks.push(task);
        }

        // Execute tasks in parallel (up to max_workers concurrent)
        let mut handles = Vec::new();
        for task in &tasks {
            if handles.len() >= self.config.max_workers {
                // Wait for first handle to complete
                let handle = handles.remove(0);
                let _ = handle.await;
            }
            if let Some(worker) = self.agents.get(&task.assigned_to) {
                let worker_id = worker.id.clone();
                let task_desc = task.description.clone();
                let handle = tokio::spawn(async move {
                    // Execute the task via the worker
                    let input = RunInput {
                        prompt: task_desc,
                        images: vec![],
                        model_override: None,
                    };
                    // Note: in production, the worker's provider would be used here
                    (worker_id, input.prompt)
                });
                handles.push(handle);
            }
        }

        // Wait for remaining handles
        for handle in handles {
            let _ = handle.await;
        }

        Ok(tasks)
    }

    /// Select the best worker for an action based on role.
    fn select_worker<'a>(&'a self, action: &Action) -> anyhow::Result<&'a SwarmAgent> {
        // Simple round-robin selection based on role
        let preferred_role = match action.name.as_str() {
            n if n.contains("test") || n.contains("review") => AgentRole::Reviewer,
            n if n.contains("research") || n.contains("search") || n.contains("find") => AgentRole::Researcher,
            _ => AgentRole::Worker,
        };

        self.agents.values()
            .find(|a| a.role == preferred_role)
            .or_else(|| self.agents.values().find(|a| a.role == AgentRole::Worker))
            .ok_or_else(|| anyhow::anyhow!("no suitable worker found for action: {}", action.name))
    }


}

#[derive(Debug, Clone)]
pub struct SwarmTaskResult {
    pub task_id: String,
    pub output: String,
    pub reasoning: String,
    pub success: bool,
}
