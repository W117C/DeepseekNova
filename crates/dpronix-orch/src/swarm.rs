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
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tracing::info;

/// Configuration for the swarm.
#[derive(Debug, Clone)]
pub struct SwarmConfig {
    pub max_workers: usize,
    pub consensus_required: bool,
    pub thinking_enabled: bool,
    pub reasoning_effort: String,
    /// Model routing table for DeepSeek V4 dual-model architecture.
    /// - Queen/Planner/Critic → Pro (strong reasoning, higher cost)
    /// - Workers → Flash (cheap, fast, adequate for focused subtasks)
    /// - Trivial tasks → Flash + thinking disabled (no CoT needed)
    pub model_routing: ModelRouting,
}

/// DeepSeek V4 model routing strategy for the Swarm.
///
/// Aligns with V4's dual-model architecture:
/// - `deepseek-v4-pro` (1.6T MoE): strong planning, cross-task consistency
/// - `deepseek-v4-flash` (284B, 13B activated): fast execution, lower cost
#[derive(Debug, Clone)]
pub struct ModelRouting {
    /// Model for Queen/Planner/Critic roles.
    pub planner_model: String,
    /// Model for Worker/Researcher roles.
    pub worker_model: String,
    /// Model for trivial / no-thinking tasks (format conversion, field extraction).
    pub trivial_model: String,
}

impl Default for ModelRouting {
    fn default() -> Self {
        Self {
            planner_model: "deepseek-v4-pro".into(),
            worker_model: "deepseek-v4-flash".into(),
            trivial_model: "deepseek-v4-flash".into(),
        }
    }
}

/// Task complexity classification for reasoning_effort routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskComplexity {
    /// Simple format conversion, field extraction — no CoT needed.
    Trivial,
    /// Standard agent task — default reasoning_effort = "high".
    Normal,
    /// Safety-critical or high-stakes decision — reasoning_effort = "max".
    Critical,
}

impl TaskComplexity {
    /// Map complexity to DeepSeek V4 reasoning_effort + thinking toggle.
    pub fn to_reasoning_config(&self) -> (bool, &'static str) {
        match self {
            TaskComplexity::Trivial => (false, "disabled"),
            TaskComplexity::Normal => (true, "high"),
            TaskComplexity::Critical => (true, "max"),
        }
    }
}

impl Default for SwarmConfig {
    fn default() -> Self {
        Self {
            max_workers: 5,
            consensus_required: false,
            thinking_enabled: true,
            reasoning_effort: "high".to_string(),
            model_routing: ModelRouting::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_swarm_config_defaults() {
        let config = SwarmConfig::default();
        assert_eq!(config.max_workers, 5);
        assert!(!config.consensus_required);
        assert!(config.thinking_enabled);
        assert_eq!(config.reasoning_effort, "high");
        assert_eq!(config.model_routing.planner_model, "deepseek-v4-pro");
        assert_eq!(config.model_routing.worker_model, "deepseek-v4-flash");
    }

    #[test]
    fn test_swarm_config_partial_override() {
        let config = SwarmConfig {
            max_workers: 3,
            ..SwarmConfig::default()
        };
        assert_eq!(config.max_workers, 3);
        assert!(config.thinking_enabled); // inherited
    }

    #[test]
    fn test_task_complexity_routing() {
        assert_eq!(
            TaskComplexity::Trivial.to_reasoning_config(),
            (false, "disabled")
        );
        assert_eq!(TaskComplexity::Normal.to_reasoning_config(), (true, "high"));
        assert_eq!(
            TaskComplexity::Critical.to_reasoning_config(),
            (true, "max")
        );
    }
}

/// The Swarm Coordinator — manages a team of agents working on a shared goal.
#[allow(dead_code)]
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

    /// Decompose a plan into sub-tasks, assign to workers, and execute.
    /// Returns the completed tasks with their outputs.
    pub async fn orchestrate(
        &mut self,
        plan: &crate::types::Plan,
    ) -> anyhow::Result<Vec<SwarmTask>> {
        // Find the queen agent
        let queen = self
            .agents
            .values()
            .find(|a| a.role == AgentRole::Queen)
            .ok_or_else(|| anyhow::anyhow!("no queen agent registered"))?;
        let queen_runner = queen.provider.clone();
        let queen_prompt = queen.system_prompt.clone();

        info!(plan_id = %plan.id, agents = self.agents.len(), "orchestrating swarm");

        // Phase 1: Decompose plan actions into tasks
        let mut tasks: Vec<SwarmTask> = Vec::new();
        for action in &plan.actions {
            if !action.delegatable {
                continue;
            }
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

        if tasks.is_empty() {
            info!("no delegatable actions in plan");
            return Ok(tasks);
        }

        // Phase 2: Execute tasks concurrently (up to max_workers)
        let max_workers = self.config.max_workers;
        let agents = self.agents.clone();
        let semaphore = Arc::new(tokio::sync::Semaphore::new(max_workers));
        let mut handles = Vec::new();

        for task in tasks.iter_mut() {
            let worker = match agents.get(&task.assigned_to).cloned() {
                Some(w) => w,
                None => {
                    task.status = SwarmTaskStatus::Failed("worker not found".into());
                    continue;
                }
            };

            let task_desc = task.description.clone();
            let permit = semaphore.clone();
            task.status = SwarmTaskStatus::InProgress;

            let handle = tokio::spawn(async move {
                let _permit = permit.acquire().await;
                let input = RunInput {
                    prompt: format!("{}\n\nTask: {}", worker.system_prompt, task_desc),
                    images: vec![],
                    model_override: None,
                };

                match worker.provider.run_stream(input).await {
                    Ok(mut stream) => {
                        let mut output_text = String::new();
                        while let Some(chunk) = stream.next().await {
                            if let Ok(dpronix_core::runner::RunEvent::Done(out)) = chunk {
                                output_text = out.text;
                            }
                        }
                        if output_text.is_empty() {
                            Err("no output from worker".to_string())
                        } else {
                            Ok(output_text)
                        }
                    }
                    Err(e) => Err(format!("{e}")),
                }
            });
            handles.push((task.id.clone(), handle));
        }

        // Phase 3: Collect results
        for (task_id, handle) in handles {
            match handle.await {
                Ok(Ok(output)) => {
                    if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
                        task.output = Some(output);
                        task.status = SwarmTaskStatus::Completed;
                    }
                }
                Ok(Err(err)) => {
                    if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
                        task.status = SwarmTaskStatus::Failed(err);
                    }
                }
                Err(e) => {
                    if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
                        task.status = SwarmTaskStatus::Failed(format!("join error: {e}"));
                    }
                }
            }
        }

        // Phase 4: Optional — Queen synthesizes results
        if self.config.consensus_required && !tasks.is_empty() {
            let results: Vec<String> = tasks.iter().filter_map(|t| t.output.clone()).collect();
            let synth_prompt = format!(
                "{}\n\nWorker results:\n{}\n\nSynthesize a final answer from these results.",
                queen_prompt,
                results.join("\n---\n")
            );
            let input = RunInput {
                prompt: synth_prompt,
                images: vec![],
                model_override: None,
            };
            if let Ok(mut stream) = queen_runner.run_stream(input).await {
                let mut synthesis = String::new();
                while let Some(chunk) = stream.next().await {
                    if let Ok(dpronix_core::runner::RunEvent::Done(out)) = chunk {
                        synthesis = out.text;
                    }
                }
                info!(len = synthesis.len(), "queen synthesis complete");
            }
        }

        let completed = tasks
            .iter()
            .filter(|t| t.status == SwarmTaskStatus::Completed)
            .count();
        info!(
            total = tasks.len(),
            completed, "swarm orchestration complete"
        );
        Ok(tasks)
    }

    /// Select the best worker for an action based on role.
    fn select_worker<'a>(&'a self, action: &Action) -> anyhow::Result<&'a SwarmAgent> {
        // Simple round-robin selection based on role
        let preferred_role = match action.name.as_str() {
            n if n.contains("test") || n.contains("review") => AgentRole::Reviewer,
            n if n.contains("research") || n.contains("search") || n.contains("find") => {
                AgentRole::Researcher
            }
            _ => AgentRole::Worker,
        };

        self.agents
            .values()
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
