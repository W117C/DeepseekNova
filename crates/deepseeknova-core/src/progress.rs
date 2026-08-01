//! # Progress Tracker — 多智能体执行的实时状态
//!
//! 线程安全的共享进度跟踪器，desktop 前端经 Tauri 命令轮询显示委派/子代理状态。
//! 自已删除的 orch crate（见 git 历史）收编而来，已解耦 SwarmConfig/Plan——仅依赖标准库 + serde。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;

/// 整体编排状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchStatus {
    Idle,
    Planning,
    Executing,
    Completed,
    Failed(String),
}

/// 单个动作的执行状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    Pending,
    InProgress,
    Completed,
    Failed(String),
}

/// 单个动作/任务的进度快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionProgress {
    pub action_id: String,
    pub name: String,
    pub description: String,
    pub status: ActionStatus,
    pub assigned_to: Option<String>,
    pub started_at: Option<u64>,
    pub completed_at: Option<u64>,
    pub output_summary: Option<String>,
    pub retry_count: u32,
}

/// 模型路由信息（前端展示用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRoutingInfo {
    pub planner_model: String,
    pub worker_model: String,
    pub thinking_enabled: bool,
    pub reasoning_effort: String,
}

impl Default for ModelRoutingInfo {
    fn default() -> Self {
        Self {
            planner_model: "deepseek-v4-pro".into(),
            worker_model: "deepseek-v4-flash".into(),
            thinking_enabled: true,
            reasoning_effort: "high".into(),
        }
    }
}

/// 完整编排进度报告——可序列化给前端。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchProgressReport {
    pub status: OrchStatus,
    pub goal: Option<String>,
    pub total_actions: usize,
    pub completed_actions: usize,
    pub failed_actions: usize,
    pub in_progress_actions: usize,
    pub elapsed_secs: f64,
    pub actions: Vec<ActionProgress>,
    pub model_routing: ModelRoutingInfo,
}

/// 线程安全的进度跟踪器，编排引擎与前端共享。
#[derive(Clone)]
pub struct ProgressTracker {
    inner: Arc<RwLock<TrackerState>>,
}

struct TrackerState {
    status: OrchStatus,
    goal: Option<String>,
    actions: HashMap<String, ActionProgress>,
    action_order: Vec<String>,
    start_time: Option<Instant>,
    model_routing: ModelRoutingInfo,
}

impl ProgressTracker {
    /// 新建空闲跟踪器。
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(TrackerState {
                status: OrchStatus::Idle,
                goal: None,
                actions: HashMap::new(),
                action_order: Vec::new(),
                start_time: None,
                model_routing: ModelRoutingInfo::default(),
            })),
        }
    }

    /// 开始一次编排（解耦：直接接受 goal + 路由信息，不再依赖 SwarmConfig）。
    pub fn start(&self, goal: &str, routing: ModelRoutingInfo) {
        let mut state = self.inner.write().unwrap_or_else(|e| e.into_inner());
        state.status = OrchStatus::Planning;
        state.goal = Some(goal.to_string());
        state.actions.clear();
        state.action_order.clear();
        state.start_time = Some(Instant::now());
        state.model_routing = routing;
    }

    /// 注册动作列表（解耦：(id, name, description) 元组，不再依赖 Plan）。
    pub fn register_actions(&self, actions: &[(String, String, String)]) {
        let mut state = self.inner.write().unwrap_or_else(|e| e.into_inner());
        state.status = OrchStatus::Executing;
        for (id, name, description) in actions {
            let progress = ActionProgress {
                action_id: id.clone(),
                name: name.clone(),
                description: description.clone(),
                status: ActionStatus::Pending,
                assigned_to: None,
                started_at: None,
                completed_at: None,
                output_summary: None,
                retry_count: 0,
            };
            state.action_order.push(id.clone());
            state.actions.insert(id.clone(), progress);
        }
    }

    /// 标记动作开始。
    pub fn mark_started(&self, action_id: &str, assigned_to: &str) {
        let mut state = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if let Some(action) = state.actions.get_mut(action_id) {
            action.status = ActionStatus::InProgress;
            action.assigned_to = Some(assigned_to.to_string());
            action.started_at = Some(now_epoch());
        }
    }

    /// 标记动作完成（输出截断至 200 字符摘要）。
    pub fn mark_completed(&self, action_id: &str, output: &str) {
        let mut state = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if let Some(action) = state.actions.get_mut(action_id) {
            action.status = ActionStatus::Completed;
            action.completed_at = Some(now_epoch());
            let summary = if output.chars().count() > 200 {
                let head: String = output.chars().take(200).collect();
                format!("{head}…")
            } else {
                output.to_string()
            };
            action.output_summary = Some(summary);
        }
    }

    /// 标记动作失败。
    pub fn mark_failed(&self, action_id: &str, reason: &str) {
        let mut state = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if let Some(action) = state.actions.get_mut(action_id) {
            action.status = ActionStatus::Failed(reason.to_string());
            action.completed_at = Some(now_epoch());
        }
    }

    /// 记录一次重试。
    pub fn record_retry(&self, action_id: &str) {
        let mut state = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if let Some(action) = state.actions.get_mut(action_id) {
            action.retry_count += 1;
            action.status = ActionStatus::InProgress;
        }
    }

    /// 标记整体编排结束。
    pub fn finish(&self) {
        let mut state = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let has_failures = state
            .actions
            .values()
            .any(|a| matches!(a.status, ActionStatus::Failed(_)));
        state.status = if has_failures {
            OrchStatus::Failed("some actions failed".into())
        } else {
            OrchStatus::Completed
        };
    }

    /// 重置为空闲。
    pub fn reset(&self) {
        let mut state = self.inner.write().unwrap_or_else(|e| e.into_inner());
        state.status = OrchStatus::Idle;
        state.goal = None;
        state.actions.clear();
        state.action_order.clear();
        state.start_time = None;
    }

    /// 生成前端进度报告。
    pub fn report(&self) -> OrchProgressReport {
        let state = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let elapsed = state
            .start_time
            .map(|s| s.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        let actions: Vec<ActionProgress> = state
            .action_order
            .iter()
            .filter_map(|id| state.actions.get(id).cloned())
            .collect();
        let completed = actions
            .iter()
            .filter(|a| a.status == ActionStatus::Completed)
            .count();
        let failed = actions
            .iter()
            .filter(|a| matches!(a.status, ActionStatus::Failed(_)))
            .count();
        let in_progress = actions
            .iter()
            .filter(|a| a.status == ActionStatus::InProgress)
            .count();
        OrchProgressReport {
            status: state.status.clone(),
            goal: state.goal.clone(),
            total_actions: actions.len(),
            completed_actions: completed,
            failed_actions: failed,
            in_progress_actions: in_progress,
            elapsed_secs: (elapsed * 10.0).round() / 10.0,
            actions,
            model_routing: state.model_routing.clone(),
        }
    }
}

impl Default for ProgressTracker {
    fn default() -> Self {
        Self::new()
    }
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_lifecycle() {
        let tracker = ProgressTracker::new();
        assert_eq!(tracker.report().status, OrchStatus::Idle);

        tracker.start("Build a REST API", ModelRoutingInfo::default());
        assert_eq!(tracker.report().status, OrchStatus::Planning);
        assert_eq!(tracker.report().goal.as_deref(), Some("Build a REST API"));

        tracker.register_actions(&[
            (
                "a1".into(),
                "create_schema".into(),
                "Create DB schema".into(),
            ),
            (
                "a2".into(),
                "write_tests".into(),
                "Write integration tests".into(),
            ),
        ]);
        let report = tracker.report();
        assert_eq!(report.status, OrchStatus::Executing);
        assert_eq!(report.total_actions, 2);
        assert_eq!(report.completed_actions, 0);

        tracker.mark_started("a1", "worker-1");
        assert_eq!(tracker.report().in_progress_actions, 1);
        tracker.mark_completed("a1", "Schema created successfully");
        assert_eq!(tracker.report().completed_actions, 1);

        tracker.mark_started("a2", "worker-2");
        tracker.mark_failed("a2", "test framework not found");
        assert_eq!(tracker.report().failed_actions, 1);

        tracker.finish();
        assert!(matches!(tracker.report().status, OrchStatus::Failed(_)));

        tracker.reset();
        assert_eq!(tracker.report().status, OrchStatus::Idle);
    }

    #[test]
    fn retry_tracking() {
        let tracker = ProgressTracker::new();
        tracker.start("test", ModelRoutingInfo::default());
        tracker.register_actions(&[("a1".into(), "flaky".into(), "Flaky action".into())]);
        tracker.mark_started("a1", "w1");
        tracker.record_retry("a1");
        tracker.record_retry("a1");
        assert_eq!(tracker.report().actions[0].retry_count, 2);
    }
}
