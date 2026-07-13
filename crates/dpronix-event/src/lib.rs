use dpronix_core::chunk::{Chunk, Usage};
use dpronix_core::graph::{NodeId, NodeOutput};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<AgentEvent>,
    hit_bytes: std::sync::Arc<AtomicU64>,
    miss_count: std::sync::Arc<AtomicU64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum EventCategory {
    Session,
    Goal,
    Plan,
    Node,
    Tool,
    Permission,
    Compaction,
    Cache,
    Profile,
    Hook,
    Checkpoint,
    Recovery,
    Notification,
    Retry,
    Turn,
    Custom,
}

impl fmt::Display for EventCategory {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Debug, Clone, Default)]
pub struct EventFilter {
    pub categories: Vec<EventCategory>,
    pub session_filter: Option<String>,
}

impl EventFilter {
    pub fn all() -> Self {
        Self::default()
    }
    pub fn categories(cats: &[EventCategory]) -> Self {
        Self {
            categories: cats.to_vec(),
            session_filter: None,
        }
    }
    pub fn for_session(id: impl Into<String>) -> Self {
        Self {
            categories: Vec::new(),
            session_filter: Some(id.into()),
        }
    }
    pub fn matches(&self, event: &AgentEvent) -> bool {
        let cat_ok = self.categories.is_empty() || self.categories.contains(&event.category());
        let session_ok = match (&self.session_filter, event.session_id()) {
            (Some(f), Some(s)) => f == s,
            _ => true,
        };
        cat_ok && session_ok
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum AgentEvent {
    SessionStarted {
        session_id: String,
        timestamp: String,
    },
    SessionEnded {
        session_id: String,
        reason: String,
        timestamp: String,
    },
    GoalStated {
        session_id: String,
        request: String,
        constraints: Vec<String>,
    },
    GoalProgressChecked {
        session_id: String,
        passed: bool,
        feedback: String,
    },
    GoalCompleted {
        session_id: String,
    },
    ModelStarted {
        provider: String,
        model: String,
    },
    ModelChunk(Chunk),
    ModelFinished {
        usage: Usage,
    },
    ToolCalled {
        call_id: String,
        name: String,
        args: String,
    },
    ToolFinished {
        call_id: String,
        name: String,
        result: String,
    },
    ToolFailed {
        call_id: String,
        name: String,
        error: String,
    },
    PermissionDenied {
        tool: String,
        reason: String,
    },
    RetryAttempt {
        attempt: u32,
        error: String,
    },
    CompactionTriggered {
        before_tokens: u32,
        after_tokens: u32,
    },
    IdleCompactionTriggered {
        idle_secs: u64,
        target_tokens: u32,
    },
    RepeatGuardTriggered {
        tool: String,
        count: u32,
    },
    CacheStat {
        hit_bytes: u64,
    },
    CacheEmpty {
        reason: String,
    },
    PlanGenerated {
        node_count: usize,
    },
    NodeStarted {
        node_id: NodeId,
    },
    NodeCompleted {
        node_id: NodeId,
        output: NodeOutput,
    },
    TurnComplete {
        turn: u32,
    },
    ProfileLoaded {
        name: String,
        model: Option<String>,
    },
    ProfileCreated {
        name: String,
    },
    ProfileDeleted {
        name: String,
    },
    HookInvoked {
        hook_name: String,
        success: bool,
    },
    CheckpointCreated {
        name: String,
        file_count: usize,
    },
    CheckpointRestored {
        name: String,
    },
    CheckpointDeleted {
        name: String,
    },
    RecoveryStarted {
        session_id: String,
    },
    RecoveryCompleted {
        session_id: String,
        recovered: bool,
    },
    NotificationSent {
        channel: String,
        success: bool,
    },
}

impl AgentEvent {
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
    pub fn new(capacity: usize) -> Self {
        let cap = if capacity == 0 { 256 } else { capacity };
        let (tx, _) = broadcast::channel(cap);
        Self {
            tx,
            hit_bytes: std::sync::Arc::new(AtomicU64::new(0)),
            miss_count: std::sync::Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn publish(&self, event: AgentEvent) {
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.tx.subscribe()
    }

    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }

    pub fn record_hit(&self, nbytes: u64) {
        self.hit_bytes.fetch_add(nbytes, Ordering::Relaxed);
    }

    pub fn record_miss(&self) {
        self.miss_count.fetch_add(1, Ordering::Relaxed);
    }

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
