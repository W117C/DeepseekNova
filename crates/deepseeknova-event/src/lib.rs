#![warn(missing_docs)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! # Event — Agent lifecycle event bus
//!
//! Defines event types emitted throughout the agent's lifecycle
//! (tool calls, model responses, errors) and provides a pub-sub bus
//! for observers.
//!
//! **库级公开 API（未接入生产路径）**：本 crate 的 `EventBus`/`AgentEvent` 当前
//! 仅被 `deepseeknova-runtime` 的组合根 `Runtime` 结构体引用，而该结构体在生产
//! 路径零使用（仅测试构造，见 AUDIT-2026-08-08 M4）。保留为库级 API 供嵌入方
//! 自行接线；若长期无人使用，建议在 M4 后续轮删除连带 `Runtime` 与
//! `context::ContextEngine` 死面。

use deepseeknova_core::chunk::{Chunk, Usage};
use deepseeknova_core::graph::{NodeId, NodeOutput};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::broadcast;

/// Agent 生命周期事件的发布-订阅总线。
///
/// 基于 `tokio::sync::broadcast` 通道：发布者的事件扇出到所有活跃订阅者；
/// 容量满时新事件会丢弃最旧的未读事件。内部维护缓存命中字节数与未命中计数，
/// 供运行时观测缓存效率（[`record_hit`](Self::record_hit) /
/// [`record_miss`](Self::record_miss)）。
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<AgentEvent>,
    hit_bytes: std::sync::Arc<AtomicU64>,
    miss_count: std::sync::Arc<AtomicU64>,
}

/// 事件大类，用于过滤与归类。`#[non_exhaustive]` 保留未来新增大类的余地。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum EventCategory {
    /// 会话级事件（开始/结束）。
    Session,
    /// 目标事件（陈述/进度检查/完成）。
    Goal,
    /// 计划生成与图节点事件。
    Plan,
    /// 图节点执行事件（已并入 [`EventCategory::Plan`]，保留以供细粒度过滤）。
    Node,
    /// 工具调用事件（调用/完成/失败）。
    Tool,
    /// 权限拒绝事件。
    Permission,
    /// 上下文压缩事件。
    Compaction,
    /// 缓存统计与重复守卫事件。
    Cache,
    /// 配置档案变更事件（加载/创建/删除）。
    Profile,
    /// 质量钩子调用事件。
    Hook,
    /// 检查点事件（创建/恢复/删除）。
    Checkpoint,
    /// 会话恢复事件。
    Recovery,
    /// 通知发送事件。
    Notification,
    /// 重试事件。
    Retry,
    /// 轮次完成事件。
    Turn,
    /// 自定义事件（预留扩展）。
    Custom,
}

impl fmt::Display for EventCategory {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

/// 事件过滤器：按大类和/或会话 ID 筛选事件。
#[derive(Debug, Clone, Default)]
pub struct EventFilter {
    /// 允许通过的大类列表；空表示不限大类。
    pub categories: Vec<EventCategory>,
    /// 仅匹配该会话 ID 的事件；`None` 表示不限会话。
    pub session_filter: Option<String>,
}

impl EventFilter {
    /// 构造一个放行所有事件的过滤器（无大类、无会话限制）。
    pub fn all() -> Self {
        Self::default()
    }
    /// 构造一个仅放行指定大类列表的过滤器（不限会话）。
    pub fn categories(cats: &[EventCategory]) -> Self {
        Self {
            categories: cats.to_vec(),
            session_filter: None,
        }
    }
    /// 构造一个仅放行指定会话 ID 事件的过滤器（不限大类）。
    pub fn for_session(id: impl Into<String>) -> Self {
        Self {
            categories: Vec::new(),
            session_filter: Some(id.into()),
        }
    }
    /// 判断给定事件是否同时满足大类与会话过滤条件。
    pub fn matches(&self, event: &AgentEvent) -> bool {
        let cat_ok = self.categories.is_empty() || self.categories.contains(&event.category());
        let session_ok = match (&self.session_filter, event.session_id()) {
            (Some(f), Some(s)) => f == s,
            _ => true,
        };
        cat_ok && session_ok
    }
}

/// Agent 生命周期中发出的所有事件类型。`#[non_exhaustive]` 保留未来新增变体的余地。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum AgentEvent {
    /// 会话开始。
    SessionStarted {
        /// 会话标识。
        session_id: String,
        /// 事件时间戳（字符串形式，由调用方约定格式）。
        timestamp: String,
    },
    /// 会话结束。
    SessionEnded {
        /// 会话标识。
        session_id: String,
        /// 结束原因。
        reason: String,
        /// 事件时间戳。
        timestamp: String,
    },
    /// 目标已陈述。
    GoalStated {
        /// 会话标识。
        session_id: String,
        /// 用户请求原文。
        request: String,
        /// 约束条件列表。
        constraints: Vec<String>,
    },
    /// 目标进度检查结果。
    GoalProgressChecked {
        /// 会话标识。
        session_id: String,
        /// 是否通过进度检查。
        passed: bool,
        /// 检查反馈说明。
        feedback: String,
    },
    /// 目标已完成。
    GoalCompleted {
        /// 会话标识。
        session_id: String,
    },
    /// 模型调用开始。
    ModelStarted {
        /// 提供商标识（如 `anthropic` / `openai`）。
        provider: String,
        /// 模型名称。
        model: String,
    },
    /// 模型流式输出的单个分片。
    ModelChunk(Chunk),
    /// 模型调用结束，携带用量统计。
    ModelFinished {
        /// 本次调用的 token 用量。
        usage: Usage,
    },
    /// 工具调用已发起。
    ToolCalled {
        /// 调用标识（与对应 `ToolFinished`/`ToolFailed` 配对）。
        call_id: String,
        /// 工具名称。
        name: String,
        /// 工具入参（JSON 字符串）。
        args: String,
    },
    /// 工具调用成功完成。
    ToolFinished {
        /// 调用标识。
        call_id: String,
        /// 工具名称。
        name: String,
        /// 工具返回结果（字符串形式）。
        result: String,
    },
    /// 工具调用失败。
    ToolFailed {
        /// 调用标识。
        call_id: String,
        /// 工具名称。
        name: String,
        /// 错误信息。
        error: String,
    },
    /// 工具调用被权限系统拒绝。
    PermissionDenied {
        /// 被拒绝的工具名称。
        tool: String,
        /// 拒绝原因。
        reason: String,
    },
    /// 一次重试发生。
    RetryAttempt {
        /// 当前重试序号（从 1 起）。
        attempt: u32,
        /// 触发重试的错误信息。
        error: String,
    },
    /// 上下文压缩被触发（达到 token 阈值）。
    CompactionTriggered {
        /// 压缩前 token 数。
        before_tokens: u32,
        /// 压缩后 token 数。
        after_tokens: u32,
    },
    /// 空闲触发的上下文压缩。
    IdleCompactionTriggered {
        /// 空闲秒数。
        idle_secs: u64,
        /// 压缩目标 token 数。
        target_tokens: u32,
    },
    /// 重复调用守卫被触发（同一工具连续调用超阈值）。
    RepeatGuardTriggered {
        /// 触发守卫的工具名称。
        tool: String,
        /// 连续调用次数。
        count: u32,
    },
    /// 缓存命中统计。
    CacheStat {
        /// 命中缓存的总字节数。
        hit_bytes: u64,
    },
    /// 缓存为空。
    CacheEmpty {
        /// 缓存为空的原因说明。
        reason: String,
    },
    /// 计划已生成。
    PlanGenerated {
        /// 计划包含的节点数。
        node_count: usize,
    },
    /// 图节点开始执行。
    NodeStarted {
        /// 节点标识。
        node_id: NodeId,
    },
    /// 图节点执行完成。
    NodeCompleted {
        /// 节点标识。
        node_id: NodeId,
        /// 节点输出。
        output: NodeOutput,
    },
    /// 一个对话轮次完成。
    TurnComplete {
        /// 轮次序号（从 1 起）。
        turn: u32,
    },
    /// 配置档案已加载。
    ProfileLoaded {
        /// 档案名称。
        name: String,
        /// 档案指定的模型（可选）。
        model: Option<String>,
    },
    /// 配置档案已创建。
    ProfileCreated {
        /// 档案名称。
        name: String,
    },
    /// 配置档案已删除。
    ProfileDeleted {
        /// 档案名称。
        name: String,
    },
    /// 质量钩子被调用。
    HookInvoked {
        /// 钩子名称。
        hook_name: String,
        /// 钩子是否成功执行。
        success: bool,
    },
    /// 检查点已创建。
    CheckpointCreated {
        /// 检查点名称。
        name: String,
        /// 包含的文件数。
        file_count: usize,
    },
    /// 检查点已恢复。
    CheckpointRestored {
        /// 检查点名称。
        name: String,
    },
    /// 检查点已删除。
    CheckpointDeleted {
        /// 检查点名称。
        name: String,
    },
    /// 会话恢复流程开始。
    RecoveryStarted {
        /// 会话标识。
        session_id: String,
    },
    /// 会话恢复流程结束。
    RecoveryCompleted {
        /// 会话标识。
        session_id: String,
        /// 是否成功恢复。
        recovered: bool,
    },
    /// 通知已发送。
    NotificationSent {
        /// 通知渠道。
        channel: String,
        /// 是否发送成功。
        success: bool,
    },
}

impl AgentEvent {
    /// 返回该事件所属的大类，供过滤与归类使用。
    pub fn category(&self) -> EventCategory {
        use AgentEvent::*;
        match self {
            SessionStarted { .. } | SessionEnded { .. } => EventCategory::Session,
            GoalStated { .. } | GoalProgressChecked { .. } | GoalCompleted { .. } => {
                EventCategory::Goal
            }
            ModelStarted { .. } | ModelChunk(_) | ModelFinished { .. } | TurnComplete { .. } => {
                EventCategory::Turn
            }
            ToolCalled { .. } | ToolFinished { .. } | ToolFailed { .. } => EventCategory::Tool,
            PermissionDenied { .. } => EventCategory::Permission,
            RetryAttempt { .. } => EventCategory::Retry,
            CompactionTriggered { .. }
            | IdleCompactionTriggered { .. }
            | CacheStat { .. }
            | CacheEmpty { .. }
            | RepeatGuardTriggered { .. } => EventCategory::Cache,
            PlanGenerated { .. } | NodeStarted { .. } | NodeCompleted { .. } => EventCategory::Plan,
            ProfileLoaded { .. } | ProfileCreated { .. } | ProfileDeleted { .. } => {
                EventCategory::Profile
            }
            HookInvoked { .. } => EventCategory::Hook,
            CheckpointCreated { .. } | CheckpointRestored { .. } | CheckpointDeleted { .. } => {
                EventCategory::Checkpoint
            }
            RecoveryStarted { .. } | RecoveryCompleted { .. } => EventCategory::Recovery,
            NotificationSent { .. } => EventCategory::Notification,
        }
    }

    /// 返回该事件关联的会话 ID（若有）；无会话语义的事件返回 `None`。
    pub fn session_id(&self) -> Option<&str> {
        use AgentEvent::*;
        match self {
            SessionStarted { session_id, .. }
            | SessionEnded { session_id, .. }
            | GoalStated { session_id, .. }
            | GoalProgressChecked { session_id, .. }
            | GoalCompleted { session_id, .. }
            | RecoveryStarted { session_id, .. }
            | RecoveryCompleted { session_id, .. } => Some(session_id),
            _ => None,
        }
    }
}

impl EventBus {
    /// 创建一个指定容量的事件总线。`capacity == 0` 时回退到默认 256；
    /// 容量满时新事件会丢弃最旧的未读事件。
    pub fn new(capacity: usize) -> Self {
        let cap = if capacity == 0 { 256 } else { capacity };
        let (tx, _) = broadcast::channel(cap);
        Self {
            tx,
            hit_bytes: std::sync::Arc::new(AtomicU64::new(0)),
            miss_count: std::sync::Arc::new(AtomicU64::new(0)),
        }
    }

    /// 向所有活跃订阅者发布事件。无订阅者或容量满时返回值为丢弃计数（此处忽略）。
    pub fn publish(&self, event: AgentEvent) {
        let _ = self.tx.send(event);
    }

    /// 订阅事件流，返回一个新的接收者。每个接收者独立消费，互不影响。
    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.tx.subscribe()
    }

    /// 返回当前活跃订阅者数量。
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }

    /// 记录一次缓存命中及其字节数，累加到命中统计。
    pub fn record_hit(&self, nbytes: u64) {
        self.hit_bytes.fetch_add(nbytes, Ordering::Relaxed);
    }

    /// 记录一次缓存未命中，累加到未命中计数。
    pub fn record_miss(&self) {
        self.miss_count.fetch_add(1, Ordering::Relaxed);
    }

    /// 返回累计的 `(命中字节数, 未命中次数)`。
    pub fn stat_totals(&self) -> (u64, u64) {
        (
            self.hit_bytes.load(Ordering::Relaxed),
            self.miss_count.load(Ordering::Relaxed),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eventbus_can_be_cloned() {
        let bus = EventBus::new(16);
        let bus2 = bus.clone();
        bus.publish(AgentEvent::TurnComplete { turn: 1 });
        assert_eq!(bus.receiver_count(), bus2.receiver_count());
    }

    #[test]
    fn agent_event_serializable() {
        let json = serde_json::to_string(&AgentEvent::TurnComplete { turn: 42 }).unwrap();
        assert!(json.contains("42"));
    }

    #[tokio::test]
    async fn subscribe_receives_event() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();
        bus.publish(AgentEvent::TurnComplete { turn: 7 });
        match rx.recv().await.unwrap() {
            AgentEvent::TurnComplete { turn: 7 } => {}
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn goal_session_and_category() {
        let e = AgentEvent::GoalStated {
            session_id: "s1".into(),
            request: "x".into(),
            constraints: vec![],
        };
        assert_eq!(e.session_id(), Some("s1"));
        assert_eq!(e.category(), EventCategory::Goal);
    }

    #[test]
    fn stat_totals_accumulate() {
        let bus = EventBus::new(16);
        bus.record_hit(1024);
        bus.record_hit(512);
        bus.record_miss();
        assert_eq!(bus.stat_totals(), (1536, 1));
    }

    #[test]
    fn filter_category_match() {
        let f = EventFilter::categories(&[EventCategory::Tool, EventCategory::Permission]);
        let tool_called = AgentEvent::ToolCalled {
            call_id: "c".into(),
            name: "x".into(),
            args: "{}".into(),
        };
        assert!(f.matches(&tool_called));
        assert!(!f.matches(&AgentEvent::TurnComplete { turn: 1 }));
    }

    #[test]
    fn filter_session_match() {
        let f = EventFilter::for_session("abc");
        let hit = AgentEvent::GoalStated {
            session_id: "abc".into(),
            request: "x".into(),
            constraints: vec![],
        };
        let miss = AgentEvent::GoalStated {
            session_id: "xyz".into(),
            request: "x".into(),
            constraints: vec![],
        };
        let tool = AgentEvent::ToolCalled {
            call_id: "c".into(),
            name: "x".into(),
            args: "{}".into(),
        };
        assert!(f.matches(&hit));
        assert!(!f.matches(&miss));
        assert!(f.matches(&tool));
    }
}
