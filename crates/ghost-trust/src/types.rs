//! Trust-runtime record types.
//!
//! Field names and defaults mirror the Python dataclasses in
//! `trust_runtime.py` exactly. Serialization goes through
//! `serde_json::Value` (BTreeMap — sorted keys) so the canonical JSON
//! matches Python's `json.dumps(sort_keys=True)`.

use serde::{Deserialize, Serialize};

/// Content snippet stored as `input_ref` / `output_ref` in journal steps.
///
/// Mirrors `_safe_snippet`: the `sha256` and `preview` are computed from
/// the Python-default JSON rendering of the redacted value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SafeSnippet {
    pub sha256: String,
    pub preview: String,
    pub truncated: bool,
}

impl SafeSnippet {
    /// Build a snippet from a JSON value, redacting first.
    /// Mirrors `_safe_snippet(value, limit=280)`.
    pub fn of(value: &serde_json::Value) -> Self {
        let redacted = ghost_core::redact_value(value);
        let text = ghost_core::python_json(&redacted);
        let preview: String = text.chars().take(280).collect();
        let truncated = text.chars().count() > 280;
        let sha256 = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(text.as_bytes());
            format!("{:x}", h.finalize())
        };
        Self {
            sha256,
            preview,
            truncated,
        }
    }

    /// Empty snippet (used when no payload is provided).
    pub fn empty() -> Self {
        Self::of(&serde_json::Value::Object(Default::default()))
    }
}

/// A durable agent run. Mirrors `DurableRun`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DurableRun {
    pub run_id: String,
    pub objective: String,
    pub source: String,
    pub status: String,
    pub created_at: f64,
    pub updated_at: f64,
    pub agent_name: String,
    pub ghost_path: String,
    pub model_provider: String,
    pub model_name: String,
    pub step_count: i64,
    pub pending_approval_count: i64,
    /// Optional redacted metadata (only present when supplied).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// A single journaled step. Mirrors `RunStepRecord` plus the hash-chain
/// fields (`previous_hash`, `record_hash`) added at write time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunStepRecord {
    pub run_id: String,
    pub step_id: String,
    pub step_type: String,
    pub status: String,
    pub input_ref: SafeSnippet,
    pub output_ref: SafeSnippet,
    pub policy_decision: serde_json::Value,
    pub idempotency_key: String,
    pub retryable: bool,
    pub timestamp: f64,
    pub duration_ms: f64,
    pub error: String,
    /// Hash of the previous record in this run's journal (`""` for first).
    #[serde(default)]
    pub previous_hash: String,
    /// `sha256(canonical_json(record without record_hash))`.
    #[serde(default)]
    pub record_hash: String,
}

/// One re-executed task, as produced by an executor during
/// [`crate::TrustRuntimeStore::replay_run`]. Mirrors the execution dicts
/// Python's `replay_run` records (PR #117: real re-execution, bug #6).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReplayExecution {
    pub task_id: String,
    pub backend_id: String,
    pub ok: bool,
    pub output: String,
    pub error: String,
}

/// Outcome comparison between an original run and its replay.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReplayComparison {
    pub original_ok_steps: usize,
    pub original_error_steps: usize,
    pub replay_ok: usize,
    pub replay_errors: usize,
    pub outcome_match: bool,
}

/// Result of [`crate::TrustRuntimeStore::replay_run`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReplayReport {
    pub replay_run_id: String,
    pub replayed_from: String,
    pub objective: String,
    pub execution_performed: bool,
    pub executions: Vec<ReplayExecution>,
    pub comparison: ReplayComparison,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn snippet_preview_truncates_at_280_chars() {
        let long = "x".repeat(5000);
        let s = SafeSnippet::of(&json!({"output": long}));
        assert!(s.truncated);
        assert_eq!(s.preview.chars().count(), 280);
        assert_eq!(s.sha256.len(), 64);
    }

    #[test]
    fn snippet_short_value_not_truncated() {
        let s = SafeSnippet::of(&json!({"ok": true}));
        assert!(!s.truncated);
    }

    #[test]
    fn snippet_redacts_secrets_before_hashing() {
        let a = SafeSnippet::of(&json!({"api_key": "sk-abcdefghijklmnop"}));
        let b = SafeSnippet::of(&json!({"api_key": "[redacted]"}));
        // Both redact to the same text, so hashes match.
        assert_eq!(a.sha256, b.sha256);
    }

    #[test]
    fn run_serializes_all_python_fields() {
        let run = DurableRun {
            run_id: "abc".into(),
            objective: "test".into(),
            source: "console".into(),
            status: "running".into(),
            created_at: 1.0,
            updated_at: 1.0,
            agent_name: "ghost-chimera".into(),
            ghost_path: "".into(),
            model_provider: "".into(),
            model_name: "".into(),
            step_count: 0,
            pending_approval_count: 0,
            metadata: None,
        };
        let v = serde_json::to_value(&run).unwrap();
        for field in [
            "run_id",
            "objective",
            "source",
            "status",
            "created_at",
            "updated_at",
            "agent_name",
            "ghost_path",
            "model_provider",
            "model_name",
            "step_count",
            "pending_approval_count",
        ] {
            assert!(v.get(field).is_some(), "missing field {field}");
        }
    }
}
