//! Typed event bus over `tokio::sync::broadcast` (architecture rule #5:
//! event-driven; UI/MCP subscribe to the same stream, no polling).

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::{self, Receiver, Sender};

use crate::task::{TaskId, TaskStatus};

/// Everything a client needs to know, pushed, never polled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineEvent {
    TaskAdded {
        id: TaskId,
        status: TaskStatus,
    },
    TaskStarted {
        id: TaskId,
    },
    TaskProgress {
        id: TaskId,
        received: u64,
        total: Option<u64>,
    },
    TaskStatusChanged {
        id: TaskId,
        status: TaskStatus,
    },
    TaskCompleted {
        id: TaskId,
    },
    TaskFailed {
        id: TaskId,
        reason: String,
    },
}

/// Broadcast hub. Lagging subscribers drop oldest events
/// (UI progress is lossy-tolerant; M2 adds coalescing).
#[derive(Clone)]
pub struct EventBus {
    tx: Sender<EngineEvent>,
}

impl EventBus {
    /// `capacity` = number of queued events before oldest are dropped.
    /// Must be non-zero: `broadcast::channel(0)` panics deep inside tokio.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "EventBus capacity must be non-zero");
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Fan out an event to every subscriber. Returns the number of receivers
    /// that received it (0 is fine — no clients connected yet).
    pub fn publish(&self, event: EngineEvent) -> usize {
        self.tx.send(event).unwrap_or(0)
    }

    /// Subscribe to the live stream.
    pub fn subscribe(&self) -> Receiver<EngineEvent> {
        self.tx.subscribe()
    }

    /// Current subscriber count — used by `/health` and tests.
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(1024)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fanout_to_multiple_subscribers() {
        let bus = EventBus::default();
        let mut s1 = bus.subscribe();
        let mut s2 = bus.subscribe();
        let event = EngineEvent::TaskAdded {
            id: TaskId::new("t1"),
            status: TaskStatus::Queued,
        };
        assert_eq!(bus.publish(event.clone()), 2);
        assert_eq!(s1.blocking_recv(), Ok(event.clone()));
        assert_eq!(s2.blocking_recv(), Ok(event));
    }

    #[test]
    fn publish_without_subscribers_is_not_an_error() {
        let bus = EventBus::new(4);
        assert_eq!(bus.receiver_count(), 0);
        assert_eq!(
            bus.publish(EngineEvent::TaskCompleted {
                id: TaskId::new("x")
            }),
            0
        );
    }

    #[test]
    fn event_serde_roundtrip() {
        let event = EngineEvent::TaskProgress {
            id: TaskId::new("t9"),
            received: 1024,
            total: Some(4096),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"task_progress\""));
        let back: EngineEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, event);
    }
}
