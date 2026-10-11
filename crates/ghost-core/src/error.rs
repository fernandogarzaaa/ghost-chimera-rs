//! Error types shared across all GHOST-Chimera crates.
//!
//! Mirrors the failure modes surfaced by the Python implementation
//! (`trust_runtime.py`, `chimera_pilot/*`, `safety_layer/*`).

use thiserror::Error;

/// Every recoverable failure in GHOST-Chimera.
#[derive(Debug, Error)]
pub enum GhostError {
    /// Filesystem / OS failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Configuration is missing or invalid.
    #[error("config error: {0}")]
    Config(String),

    /// A policy engine denied the action.
    #[error("policy denied: {0}")]
    PolicyDenied(String),

    /// The action needs a human approval checkpoint first.
    #[error("approval required: {0}")]
    ApprovalRequired(String),

    /// No backend can run the requested task.
    #[error("backend unavailable: {0}")]
    BackendUnavailable(String),

    /// A deterministic task could not be fulfilled honestly.
    /// The message always starts with `deterministic_unfulfilled`.
    #[error("{0}")]
    Unfulfilled(String),

    /// A journaled run, order, session, or approval was not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// JSON serialization / deserialization failure.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// A trust-journal hash chain failed verification.
    #[error("integrity error: {0}")]
    Integrity(String),
}

/// Convenience alias used across crates.
pub type Result<T> = std::result::Result<T, GhostError>;
