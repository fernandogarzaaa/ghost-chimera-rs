//! Shared primitives for GHOST-Chimera.
//!
//! Mirrors `ghostchimera/config.py`, `redaction.py`, and the id/time helpers
//! in `trust_runtime.py`. No domain logic lives here.

pub mod config;
pub mod error;
pub mod ids;
pub mod json;
pub mod redact;
pub mod time;

pub use config::GhostConfig;
pub use error::{GhostError, Result};
pub use ids::{new_id, stable_id};
pub use json::{canonical_json, hash_payload, python_json};
pub use redact::{redact_text, redact_value};
pub use time::now;
