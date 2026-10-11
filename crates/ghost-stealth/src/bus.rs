//! Async event bus for the Stealth Loop.
//!
//! A tokio broadcast channel: every subscriber sees every event.
//! Mirrors the Python `EventBus` fan-out semantics.

use tokio::sync::broadcast;

use crate::event::Event;

/// Broadcast bus carrying [`Event`]s to all subscribers.
#[derive(Debug, Clone)]
pub struct EventBus {
    sender: broadcast::Sender<Event>,
}

impl EventBus {
    /// Create a bus with the given channel capacity.
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }

    /// Publish an event to all subscribers. Returns the receiver count.
    /// Lagged receivers drop the event (same as Python's best-effort fan-out).
    pub fn publish(&self, event: Event) -> usize {
        self.sender.send(event).unwrap_or(0)
    }

    /// Subscribe to the event stream.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.sender.subscribe()
    }

    /// Number of active subscribers.
    pub fn receiver_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{new_event, NewEventArgs};

    #[tokio::test]
    async fn publish_reaches_subscribers() {
        let bus = EventBus::new(16);
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();
        let n = bus.publish(new_event("file.created", NewEventArgs::new("test")));
        assert_eq!(n, 2);
        let e1 = rx1.recv().await.unwrap();
        let e2 = rx2.recv().await.unwrap();
        assert_eq!(e1.event_type, "file.created");
        assert_eq!(e2.event_id, e1.event_id);
    }

    #[tokio::test]
    async fn no_subscribers_no_panic() {
        let bus = EventBus::new(16);
        let n = bus.publish(new_event("x", NewEventArgs::new("test")));
        assert_eq!(n, 0);
    }
}
