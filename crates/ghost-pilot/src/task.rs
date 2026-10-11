//! Task model: what the pilot executes.
//!
//! Mirrors `chimera_pilot/task_ir.py`.

use serde::{Deserialize, Serialize};

/// The kind of work a task represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Reasoning,
    ToolCall,
    RagQuery,
}

/// A compiled unit of work for a backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSpec {
    /// Stable task id.
    pub id: String,
    /// What kind of work this is.
    pub kind: TaskKind,
    /// The natural-language objective.
    pub objective: String,
}

impl TaskSpec {
    /// Create a task with an explicit id.
    pub fn new(id: impl Into<String>, kind: TaskKind, objective: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind,
            objective: objective.into(),
        }
    }
}
