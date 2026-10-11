//! ChimeraPilotKernel for GHOST-Chimera.
//!
//! Mirrors `ghostchimera/chimera_pilot/`: task compilation, backend
//! selection, and honest execution (PR #116: never false success).

pub mod backends;
pub mod kernel;
pub mod task;

pub use backends::deterministic::{DeterministicBackend, UNFULFILLED_ERROR};
pub use backends::{Backend, BackendHealth, ExecutionResult};
pub use kernel::{ChimeraPilotKernel, KernelOptions, PilotExecution};
pub use task::{TaskKind, TaskSpec};
