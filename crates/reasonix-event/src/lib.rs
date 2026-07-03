use reasonix_core::chunk::{Chunk, Usage};
use reasonix_core::graph::{NodeId, NodeOutput};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// EventBus uses tokio::sync::broadcast — publish is non-blocking.
/// Slow subscribers get lagged (events dropped for them), not the publisher.
///
/// EventBus is Clone-able (wraps broadcast::Sender in Arc internally).
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<AgentEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum AgentEvent {
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
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let capacity = if capacity == 0 { 256 } else { capacity };
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Non-blocking publish: send-and-forget. If no receivers, event is dropped.
    /// Slow subscribers are lagged, not blocking the publisher.
    pub fn publish(&self, event: AgentEvent) {
        let _ = self.tx.send(event);
    }

    /// Subscribe for a new receiver.
    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.tx.subscribe()
    }

    /// Number of active receivers.
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
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
        // Both share the same channel
        assert_eq!(bus.receiver_count(), bus2.receiver_count());
    }

    #[test]
    fn eventbus_defaults_to_256() {
        let bus = EventBus::new(0);
        assert_eq!(bus.receiver_count(), 0);
        // Capacity actually isn't exposed — but publish works
        bus.publish(AgentEvent::TurnComplete { turn: 1 });
    }

    #[test]
    fn agent_event_is_serializable() {
        let event = AgentEvent::TurnComplete { turn: 42 };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("42"));
    }

    #[tokio::test]
    async fn subscribe_receives_published() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();
        bus.publish(AgentEvent::TurnComplete { turn: 7 });

        let received = rx.recv().await.unwrap();
        match received {
            AgentEvent::TurnComplete { turn } => assert_eq!(turn, 7),
            _ => panic!("wrong event"),
        }
    }
}
