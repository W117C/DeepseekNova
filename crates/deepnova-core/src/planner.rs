use crate::graph::{Action, ExecutionGraph, ExecutionNode};
use crate::registry::Planner;
use async_trait::async_trait;

/// SimplePlanner produces a basic linear execution plan.
/// For every goal it generates: Think(analyze) → Think(execute) → Reflect.
///
/// This is intentionally minimal. An LLM-driven planner is implemented by
/// `CoordinatorRunner` in `deepnova-agent` (see Phase 3).
pub struct SimplePlanner;

#[async_trait]
impl Planner for SimplePlanner {
    fn name(&self) -> &str {
        "simple"
    }

    async fn plan(&self, goal: &str) -> anyhow::Result<ExecutionGraph> {
        let entry = "analyze".to_string();

        let mut graph = ExecutionGraph::new(entry.clone());

        // Step 1: Analyze the goal
        graph.add_node(ExecutionNode::new(
            "analyze",
            Action::Think {
                prompt: format!("Analyze the following goal and identify the key tasks:\n\n{goal}"),
            },
        ));

        // Step 2: Execute
        graph.add_node(ExecutionNode::new(
            "execute",
            Action::Think {
                prompt: format!("Execute the following goal step by step:\n\n{goal}"),
            },
        ));

        // Step 3: Reflect on completeness
        graph.add_node(ExecutionNode::new(
            "reflect",
            Action::Reflect {
                criteria: vec![
                    "Goal achieved?".to_string(),
                    "All steps completed?".to_string(),
                    "Output is correct?".to_string(),
                ],
            },
        ));

        // Wire dependencies: analyze → execute → reflect
        graph.add_edge("analyze".into(), "execute".into(), None);
        graph.add_edge("execute".into(), "reflect".into(), None);

        Ok(graph)
    }
}

/// TaskPlanner breaks a goal into sub-tasks and plans them in parallel
/// where possible.
pub struct TaskPlanner;

#[async_trait]
impl Planner for TaskPlanner {
    fn name(&self) -> &str {
        "task"
    }

    async fn plan(&self, goal: &str) -> anyhow::Result<ExecutionGraph> {
        let entry = "think".to_string();
        let mut graph = ExecutionGraph::new(entry.clone());

        // Phase 1: Think about the goal
        graph.add_node(ExecutionNode::new(
            "think",
            Action::Think {
                prompt: format!(
                    "Break down the following goal into independent sub-tasks:\n\n{goal}"
                ),
            },
        ));

        // Phase 2: Execute sub-tasks in parallel — these are fixed template
        // nodes; real decomposition is handled by CoordinatorRunner in
        // deepnova-agent which calls the LLM to generate a dynamic plan.
        graph.add_node(ExecutionNode::new(
            "subtask_a",
            Action::Think {
                prompt: format!("Sub-task A for: {goal}"),
            },
        ));

        graph.add_node(ExecutionNode::new(
            "subtask_b",
            Action::Think {
                prompt: format!("Sub-task B for: {goal}"),
            },
        ));

        // Phase 3: Synthesize results
        graph.add_node(ExecutionNode::new(
            "synthesize",
            Action::Reflect {
                criteria: vec![
                    "All sub-tasks completed?".to_string(),
                    "Results are consistent?".to_string(),
                    "Goal is fully addressed?".to_string(),
                ],
            },
        ));

        // think → subtask_a, subtask_b → synthesize
        graph.add_edge("think".into(), "subtask_a".into(), None);
        graph.add_edge("think".into(), "subtask_b".into(), None);
        graph.add_edge("subtask_a".into(), "synthesize".into(), None);
        graph.add_edge("subtask_b".into(), "synthesize".into(), None);

        Ok(graph)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn simple_planner_creates_linear_plan() {
        let planner = SimplePlanner;
        let graph = planner.plan("test goal").await.unwrap();

        assert_eq!(graph.entry, "analyze");
        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.edges.len(), 2);
    }

    #[test]
    fn simple_planner_has_correct_name() {
        assert_eq!(SimplePlanner.name(), "simple");
    }

    #[tokio::test]
    async fn task_planner_creates_diamond_plan() {
        let planner = TaskPlanner;
        let graph = planner.plan("complex task").await.unwrap();

        assert_eq!(graph.entry, "think");
        assert_eq!(graph.nodes.len(), 4);
        assert_eq!(graph.edges.len(), 4);
    }

    #[test]
    fn task_planner_has_correct_name() {
        assert_eq!(TaskPlanner.name(), "task");
    }
}
