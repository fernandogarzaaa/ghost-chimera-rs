//! Normalized events for the Stealth Loop.
//!
//! Mirrors `ghostchimera/stealth/events.py::new_event`.

use serde::{Deserialize, Serialize};

/// A normalized ambient event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    pub event_id: String,
    pub event_type: String,
    pub timestamp: f64,
    pub source: String,
    pub actor: String,
    pub payload: serde_json::Value,
    pub provenance: serde_json::Value,
    pub correlation_id: String,
    pub session_id: String,
    pub privacy_classification: String,
    pub confidence: f64,
}

/// Arguments for [`new_event`].
#[derive(Debug, Clone, Default)]
pub struct NewEventArgs {
    pub source: String,
    pub actor: String,
    pub payload: serde_json::Value,
    pub provenance: serde_json::Value,
    pub correlation_id: String,
    pub session_id: String,
    pub privacy_classification: String,
    pub confidence: f64,
    pub event_id: Option<String>,
    pub timestamp: Option<f64>,
}

impl NewEventArgs {
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            payload: serde_json::Value::Object(Default::default()),
            provenance: serde_json::Value::Object(Default::default()),
            privacy_classification: "internal".into(),
            confidence: 1.0,
            ..Default::default()
        }
    }
}

/// Build a normalized event with sane defaults.
/// Mirrors `new_event(event_type, source=..., ...)`.
pub fn new_event(event_type: &str, args: NewEventArgs) -> Event {
    let event_id = args.event_id.unwrap_or_else(|| {
        // Python: f"{prefix}-{uuid.uuid4().hex[:12]}" — we use a stable id
        // from type+timestamp+source (no uuid dependency in Phase 1).
        format!(
            "evt-{}",
            ghost_core::stable_id(
                &[
                    Some(event_type),
                    Some(&args.source),
                    Some(&ghost_core::now().to_string())
                ],
                12
            )
        )
    });
    Event {
        event_id,
        event_type: event_type.trim().to_lowercase(),
        timestamp: args.timestamp.unwrap_or_else(ghost_core::now),
        source: args.source,
        actor: args.actor,
        payload: args.payload,
        provenance: args.provenance,
        correlation_id: args.correlation_id,
        session_id: args.session_id,
        privacy_classification: if args.privacy_classification.is_empty() {
            "internal".into()
        } else {
            args.privacy_classification
        },
        confidence: args.confidence.clamp(0.0, 1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_type_and_clamps_confidence() {
        let e = new_event(
            "  Agent.Prompt_Submitted ",
            NewEventArgs {
                confidence: 2.5,
                ..NewEventArgs::new("test")
            },
        );
        assert_eq!(e.event_type, "agent.prompt_submitted");
        assert_eq!(e.confidence, 1.0);
    }

    #[test]
    fn defaults_are_sane() {
        let e = new_event("file.created", NewEventArgs::new("watcher"));
        assert!(e.event_id.starts_with("evt-"));
        assert_eq!(e.privacy_classification, "internal");
        assert_eq!(e.confidence, 1.0);
        assert!(e.timestamp > 0.0);
    }

    #[test]
    fn explicit_id_and_timestamp_respected() {
        let args = NewEventArgs {
            event_id: Some("evt-custom".into()),
            timestamp: Some(1234.5),
            ..NewEventArgs::new("s")
        };
        let e = new_event("x", args);
        assert_eq!(e.event_id, "evt-custom");
        assert_eq!(e.timestamp, 1234.5);
    }
}
