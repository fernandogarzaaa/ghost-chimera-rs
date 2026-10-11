//! Backend abstraction for the pilot kernel.
//!
//! Mirrors `chimera_pilot/backends/base.py`.

pub mod deterministic;

pub use deterministic::DeterministicBackend;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::task::TaskSpec;

/// Health/capability snapshot for a backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendHealth {
    /// Whether the backend can run right now.
    pub available: bool,
    /// Expected reliability 0.0..=1.0.
    pub reliability: f64,
    /// Expected latency in milliseconds.
    pub latency_ms: u64,
    /// Expected cost in USD per run.
    pub estimated_cost_usd: f64,
}

impl Default for BackendHealth {
    fn default() -> Self {
        Self {
            available: true,
            reliability: 1.0,
            latency_ms: 1,
            estimated_cost_usd: 0.0,
        }
    }
}

/// The result of executing one task on a backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    /// Backend that produced this result.
    pub backend_id: String,
    /// Task that was executed.
    pub task_id: String,
    /// True only when the task genuinely succeeded.
    pub ok: bool,
    /// The produced output (empty when `ok` is false).
    pub output: String,
    /// Machine-readable failure reason (empty when `ok` is true).
    pub error: String,
    /// Backend-reported metrics.
    #[serde(default)]
    pub metrics: HashMap<String, serde_json::Value>,
}

/// A pilot backend: something that can execute tasks.
pub trait Backend: Send + Sync {
    /// Stable backend id, e.g. `"deterministic.local"`.
    fn id(&self) -> &str;

    /// Current health snapshot.
    fn probe(&self) -> BackendHealth;

    /// Whether this backend can attempt the task.
    fn can_run(&self, task: &TaskSpec) -> bool;

    /// Execute the task. Must never report false success.
    fn execute(&self, task: &TaskSpec) -> ExecutionResult;
}
