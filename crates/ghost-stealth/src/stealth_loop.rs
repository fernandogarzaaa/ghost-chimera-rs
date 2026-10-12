//! The Stealth Loop.
//!
//! Pipeline: EVENT → UNDERSTAND → UPDATE STATE → RECALL EXPERIENCE
//! → MATCH WORKFLOW → PREDICT → DECIDE → PREPARE/INJECT/ACT
//! → OBSERVE OUTCOME → LEARN.
//!
//! Phase 1 implements the real event intake, world-state update,
//! intervention lifecycle, and outcome observation. The prediction
//! engine, workflow learner, and experience graph are represented by
//! their interfaces with straightforward in-memory behavior; full
//! ML-backed implementations arrive in later phases.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use ghost_core::GhostError;
use serde::{Deserialize, Serialize};

use crate::bus::EventBus;
use crate::event::Event;

/// Outcome of an intervention, observed after the turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InterventionOutcome {
    Useful,
    Ignored,
    Harmful,
}

impl InterventionOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Useful => "useful",
            Self::Ignored => "ignored",
            Self::Harmful => "harmful",
        }
    }
}

/// Lifecycle state of an intervention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterventionState {
    Proposed,
    Ready,
    Injected,
    Observed,
}

/// A single intervention decision.
#[derive(Debug, Clone)]
pub struct Intervention {
    pub intervention_id: String,
    pub event_id: String,
    pub host: String,
    pub context: String,
    pub state: InterventionState,
    pub outcome: Option<InterventionOutcome>,
}

/// The last decision the loop made (mirrors `loop.last_result`).
#[derive(Debug, Clone)]
pub struct LoopDecision {
    pub intervention_id: String,
    pub decision: String,
}

/// A single action to execute as part of an ACT decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub action_id: String,
    pub kind: String,
    pub payload: serde_json::Value,
}

/// The recorded result of executing one action. `ok` reflects the real
/// executor outcome — it is never fabricated (F1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionResult {
    pub action: Action,
    pub ok: bool,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
}

/// In-memory world state: recent events indexed by type.
#[derive(Debug, Default)]
struct WorldState {
    recent: VecDeque<Event>,
    by_type: HashMap<String, usize>,
    capacity: usize,
}

impl WorldState {
    fn new(capacity: usize) -> Self {
        Self {
            recent: VecDeque::with_capacity(capacity),
            by_type: HashMap::new(),
            capacity,
        }
    }

    /// UNDERSTAND + UPDATE STATE: normalize and index the event.
    fn observe(&mut self, event: Event) {
        *self.by_type.entry(event.event_type.clone()).or_insert(0) += 1;
        if self.recent.len() >= self.capacity {
            if let Some(old) = self.recent.pop_front() {
                if let Some(count) = self.by_type.get_mut(&old.event_type) {
                    *count = count.saturating_sub(1);
                }
            }
        }
        self.recent.push_back(event);
    }

    fn count_of(&self, event_type: &str) -> usize {
        self.by_type.get(event_type).copied().unwrap_or(0)
    }
}

/// The ambient Stealth Loop.
pub struct StealthLoop {
    bus: EventBus,
    world: Arc<Mutex<WorldState>>,
    interventions: Arc<Mutex<HashMap<String, Intervention>>>,
    last_decision: Arc<Mutex<Option<LoopDecision>>>,
    outcomes: Arc<Mutex<Vec<(String, InterventionOutcome)>>>,
}

impl StealthLoop {
    /// Create a loop with the given bus capacity and world-state window.
    pub fn new(bus_capacity: usize, world_capacity: usize) -> Self {
        Self {
            bus: EventBus::new(bus_capacity),
            world: Arc::new(Mutex::new(WorldState::new(world_capacity))),
            interventions: Arc::new(Mutex::new(HashMap::new())),
            last_decision: Arc::new(Mutex::new(None)),
            outcomes: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Access the event bus (for hosts to subscribe).
    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    /// EVENT → UNDERSTAND → UPDATE STATE → … → DECIDE.
    ///
    /// Emits the event on the bus, updates world state, and runs the
    /// decide step. Returns the intervention id when the loop decided
    /// to act, `None` when it stayed silent.
    pub fn emit(&self, event: Event) -> Option<String> {
        // EVENT: fan out.
        self.bus.publish(event.clone());
        // UNDERSTAND + UPDATE STATE.
        self.world.lock().unwrap().observe(event.clone());
        // RECALL EXPERIENCE → MATCH WORKFLOW → PREDICT → DECIDE.
        // Phase 1 decision rule (the full prediction engine lands later):
        // act on high-confidence agent prompt/tool events.
        let should_act = event.confidence >= 0.5
            && (event.event_type == "agent.prompt_submitted"
                || event.event_type == "agent.tool_called");
        if !should_act {
            return None;
        }
        let intervention_id = format!(
            "ivn-{}",
            ghost_core::stable_id(&[Some(&event.event_id), Some("intervene")], 12)
        );
        let decision = LoopDecision {
            intervention_id: intervention_id.clone(),
            decision: "inject_context".to_string(),
        };
        // PREPARE: register the intervention as proposed.
        self.interventions.lock().unwrap().insert(
            intervention_id.clone(),
            Intervention {
                intervention_id: intervention_id.clone(),
                event_id: event.event_id,
                host: event.source,
                context: String::new(),
                state: InterventionState::Proposed,
                outcome: None,
            },
        );
        *self.last_decision.lock().unwrap() = Some(decision);
        Some(intervention_id)
    }

    /// PREPARE → INJECT: mark prepared and record the injected context.
    pub fn inject(&self, intervention_id: &str, context: &str) -> Result<(), GhostError> {
        let mut interventions = self.interventions.lock().unwrap();
        let ivn = interventions
            .get_mut(intervention_id)
            .ok_or_else(|| GhostError::NotFound(format!("intervention {intervention_id}")))?;
        ivn.context = context.to_string();
        ivn.state = InterventionState::Injected;
        Ok(())
    }

    /// OBSERVE OUTCOME → LEARN: record what happened.
    pub fn observe_outcome(
        &self,
        intervention_id: &str,
        outcome: InterventionOutcome,
    ) -> Result<(), GhostError> {
        let mut interventions = self.interventions.lock().unwrap();
        let ivn = interventions
            .get_mut(intervention_id)
            .ok_or_else(|| GhostError::NotFound(format!("intervention {intervention_id}")))?;
        ivn.outcome = Some(outcome);
        ivn.state = InterventionState::Observed;
        self.outcomes
            .lock()
            .unwrap()
            .push((intervention_id.to_string(), outcome));
        Ok(())
    }

    /// ACT: execute an intervention's actions through the provided executor.
    ///
    /// F1 fix: the executor is a REQUIRED parameter (not `Option`). There is
    /// no code path where an ACT decision records success without executing.
    /// Each action's real result determines the outcome; failures are recorded
    /// honestly, never papered over with `ok: true`.
    pub fn execute_actions(
        &self,
        intervention_id: &str,
        actions: &[Action],
        executor: &dyn Fn(&Action) -> Result<serde_json::Value, GhostError>,
    ) -> Result<Vec<ActionResult>, GhostError> {
        // Validate BEFORE touching the executor: an unknown intervention
        // must not cause real side effects that can never be recorded.
        if !self
            .interventions
            .lock()
            .unwrap()
            .contains_key(intervention_id)
        {
            return Err(GhostError::NotFound(format!(
                "intervention {intervention_id}"
            )));
        }
        // An ACT with nothing to execute is a no-op, and a no-op must never
        // be recorded as a Useful outcome (F1: "never record SUCCESSFUL on
        // no-op"). Refuse explicitly; no outcome is recorded.
        if actions.is_empty() {
            return Err(GhostError::PolicyDenied(format!(
                "intervention {intervention_id}: ACT decision has no actions to execute"
            )));
        }
        let mut results = Vec::with_capacity(actions.len());
        let mut any_failed = false;
        for action in actions {
            match executor(action) {
                Ok(value) => results.push(ActionResult {
                    action: action.clone(),
                    ok: true,
                    result: Some(value),
                    error: None,
                }),
                Err(err) => {
                    any_failed = true;
                    results.push(ActionResult {
                        action: action.clone(),
                        ok: false,
                        result: None,
                        error: Some(err.to_string()),
                    });
                }
            }
        }
        let outcome = if any_failed {
            InterventionOutcome::Harmful
        } else {
            InterventionOutcome::Useful
        };
        self.observe_outcome(intervention_id, outcome)?;
        Ok(results)
    }

    /// The most recent decision, if any.
    pub fn last_result(&self) -> Option<LoopDecision> {
        self.last_decision.lock().unwrap().clone()
    }

    /// Standing context for injection (recent high-signal events).
    pub fn get_standing_context(&self) -> String {
        let world = self.world.lock().unwrap();
        let mut lines = Vec::new();
        for event in world.recent.iter().rev().take(10) {
            lines.push(format!(
                "[{}] {}: {}",
                event.timestamp, event.event_type, event.source
            ));
        }
        lines.join("\n")
    }

    /// Number of observed events of a type (for tests and hosts).
    pub fn event_count(&self, event_type: &str) -> usize {
        self.world.lock().unwrap().count_of(event_type)
    }

    /// Recorded (intervention_id, outcome) pairs, in order.
    pub fn outcomes(&self) -> Vec<(String, InterventionOutcome)> {
        self.outcomes.lock().unwrap().clone()
    }

    /// Look up an intervention by id.
    pub fn intervention(&self, id: &str) -> Option<Intervention> {
        self.interventions.lock().unwrap().get(id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{new_event, NewEventArgs};

    fn prompt_event() -> Event {
        new_event("agent.prompt_submitted", NewEventArgs::new("openclaw"))
    }

    #[test]
    fn emit_decides_on_agent_prompts() {
        let ste = StealthLoop::new(16, 100);
        let id = ste.emit(prompt_event());
        assert!(id.is_some());
        let decision = ste.last_result().unwrap();
        assert_eq!(decision.decision, "inject_context");
        assert_eq!(decision.intervention_id, id.unwrap());
    }

    #[test]
    fn emit_stays_silent_on_low_signal() {
        let ste = StealthLoop::new(16, 100);
        let id = ste.emit(new_event("file.created", NewEventArgs::new("watcher")));
        assert!(id.is_none());
        assert!(ste.last_result().is_none());
        // But world state still observed it.
        assert_eq!(ste.event_count("file.created"), 1);
    }

    #[test]
    fn inject_and_observe_lifecycle() {
        let ste = StealthLoop::new(16, 100);
        let id = ste.emit(prompt_event()).unwrap();
        ste.inject(&id, "relevant context").unwrap();
        let ivn = ste.intervention(&id).unwrap();
        assert_eq!(ivn.state, InterventionState::Injected);
        assert_eq!(ivn.context, "relevant context");

        ste.observe_outcome(&id, InterventionOutcome::Useful)
            .unwrap();
        let ivn = ste.intervention(&id).unwrap();
        assert_eq!(ivn.state, InterventionState::Observed);
        assert_eq!(ivn.outcome, Some(InterventionOutcome::Useful));
        assert_eq!(ste.outcomes(), vec![(id, InterventionOutcome::Useful)]);
    }

    #[test]
    fn unknown_intervention_errors() {
        let ste = StealthLoop::new(16, 100);
        assert!(ste.inject("nope", "ctx").is_err());
        assert!(ste
            .observe_outcome("nope", InterventionOutcome::Ignored)
            .is_err());
    }

    #[test]
    fn standing_context_lists_recent_events() {
        let ste = StealthLoop::new(16, 100);
        ste.emit(prompt_event());
        let ctx = ste.get_standing_context();
        assert!(ctx.contains("agent.prompt_submitted"));
    }

    #[test]
    fn execute_actions_records_real_results_not_fabricated_success() {
        // F1: with a failing executor, results are ok:false and the outcome
        // is NOT Useful. There is no path that records success without execution.
        let ste = StealthLoop::new(16, 100);
        ste.emit(prompt_event());
        let decision = ste.last_result().expect("decision");
        let actions = vec![Action {
            action_id: "a1".to_string(),
            kind: "test".to_string(),
            payload: serde_json::json!({}),
        }];
        let failing = |_: &Action| -> Result<serde_json::Value, GhostError> {
            Err(GhostError::BackendUnavailable("no backend".to_string()))
        };
        let results = ste
            .execute_actions(&decision.intervention_id, &actions, &failing)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(!results[0].ok);
        assert!(results[0].error.is_some());
        // Outcome reflects the real failure, not fabricated success.
        assert_ne!(
            ste.outcomes(),
            vec![(
                decision.intervention_id.clone(),
                InterventionOutcome::Useful
            )]
        );
    }

    #[test]
    fn execute_actions_success_records_useful() {
        let ste = StealthLoop::new(16, 100);
        ste.emit(prompt_event());
        let decision = ste.last_result().expect("decision");
        let actions = vec![Action {
            action_id: "a1".to_string(),
            kind: "test".to_string(),
            payload: serde_json::json!({}),
        }];
        let ok_exec = |_: &Action| -> Result<serde_json::Value, GhostError> {
            Ok(serde_json::json!({"done": true}))
        };
        let results = ste
            .execute_actions(&decision.intervention_id, &actions, &ok_exec)
            .unwrap();
        assert!(results[0].ok);
        assert_eq!(
            ste.outcomes(),
            vec![(decision.intervention_id, InterventionOutcome::Useful)]
        );
    }

    fn one_action() -> Vec<Action> {
        vec![Action {
            action_id: "a1".to_string(),
            kind: "test".to_string(),
            payload: serde_json::json!({}),
        }]
    }

    #[test]
    fn execute_actions_with_no_actions_records_nothing() {
        // F1: an ACT with zero actions is a no-op; it must not be
        // recorded as a Useful (or any) outcome.
        let ste = StealthLoop::new(16, 100);
        let id = ste.emit(prompt_event()).unwrap();
        let never = |_: &Action| -> Result<serde_json::Value, GhostError> {
            panic!("executor must not run for an empty action list")
        };
        let err = ste.execute_actions(&id, &[], &never).unwrap_err();
        assert!(matches!(err, GhostError::PolicyDenied(_)), "{err}");
        assert!(ste.outcomes().is_empty());
        let ivn = ste.intervention(&id).unwrap();
        assert_eq!(ivn.outcome, None);
        assert_ne!(ivn.state, InterventionState::Observed);
    }

    #[test]
    fn execute_actions_unknown_intervention_never_runs_executor() {
        let ste = StealthLoop::new(16, 100);
        let calls = std::cell::Cell::new(0);
        let counting = |_: &Action| -> Result<serde_json::Value, GhostError> {
            calls.set(calls.get() + 1);
            Ok(serde_json::json!({}))
        };
        let err = ste
            .execute_actions("nope", &one_action(), &counting)
            .unwrap_err();
        assert!(matches!(err, GhostError::NotFound(_)), "{err}");
        assert_eq!(calls.get(), 0, "no side effects for unknown intervention");
        assert!(ste.outcomes().is_empty());
    }

    #[test]
    fn execute_actions_partial_failure_is_harmful_and_keeps_order() {
        let ste = StealthLoop::new(16, 100);
        let id = ste.emit(prompt_event()).unwrap();
        let actions = vec![
            Action {
                action_id: "ok".to_string(),
                kind: "test".to_string(),
                payload: serde_json::json!({}),
            },
            Action {
                action_id: "bad".to_string(),
                kind: "test".to_string(),
                payload: serde_json::json!({}),
            },
        ];
        let exec = |a: &Action| -> Result<serde_json::Value, GhostError> {
            if a.action_id == "bad" {
                Err(GhostError::BackendUnavailable("boom".to_string()))
            } else {
                Ok(serde_json::json!({"done": true}))
            }
        };
        let results = ste.execute_actions(&id, &actions, &exec).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results[0].ok);
        assert_eq!(results[0].result, Some(serde_json::json!({"done": true})));
        assert!(results[0].error.is_none());
        assert!(!results[1].ok);
        assert!(results[1].result.is_none());
        assert!(results[1].error.as_deref().unwrap().contains("boom"));
        assert_eq!(ste.outcomes(), vec![(id, InterventionOutcome::Harmful)]);
    }
}
