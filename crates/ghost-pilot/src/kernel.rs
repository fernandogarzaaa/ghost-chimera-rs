//! The pilot kernel: objective -> tasks -> backend execution.
//!
//! Mirrors `chimera_pilot/kernel.py` (PR #116 honesty semantics).
//!
//! **Honesty rule (critical):** the kernel NEVER reports success when no
//! backend fulfilled the objective. Unfulfillable objectives produce
//! `ok=false` with a machine-readable reason starting with
//! `deterministic_unfulfilled`.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use ghost_core::GhostError;

use crate::backends::deterministic::UNFULFILLED_ERROR;
use crate::backends::{Backend, DeterministicBackend, ExecutionResult};
use crate::task::{TaskKind, TaskSpec};
use ghost_providers::ModelProvider;

/// Options for building a kernel, mirroring `ChimeraPilotKernel.default()`.
#[derive(Debug, Clone)]
pub struct KernelOptions {
    /// Register the deterministic offline backend.
    pub include_deterministic_backend: bool,
    /// Let the deterministic backend actually fulfill tasks it understands.
    pub deterministic_fulfill: bool,
    /// Use a model provider backend when one is available.
    pub include_model_provider_backend: bool,
    /// Autonomy level string (e.g. "supervised").
    pub autonomy_level: String,
}

impl Default for KernelOptions {
    fn default() -> Self {
        Self {
            include_deterministic_backend: true,
            deterministic_fulfill: false,
            include_model_provider_backend: false,
            autonomy_level: "supervised".to_string(),
        }
    }
}

/// One executed task, serialized to the Python-compatible envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PilotExecution {
    /// Task that was executed.
    pub task_id: String,
    /// Backend that ran it.
    pub backend_id: String,
    /// True only when the objective was genuinely fulfilled.
    pub ok: bool,
    /// Mirrors Python's `task_completed` flag (PR #116).
    pub task_completed: bool,
    /// Produced output (empty unless `ok`).
    pub output: String,
    /// Machine-readable failure reason (empty when `ok`).
    pub error: String,
}

impl PilotExecution {
    fn from_result(result: ExecutionResult) -> Self {
        // Honesty: ok requires the backend's ok AND no error text.
        // A backend claiming ok=true with an error message is lying.
        let ok = result.ok && result.error.is_empty();
        Self {
            task_id: result.task_id,
            backend_id: result.backend_id,
            ok,
            task_completed: ok,
            output: if ok { result.output } else { String::new() },
            error: if ok { String::new() } else { result.error },
        }
    }

    /// An execution that never ran: no backend could fulfill the objective.
    fn unfulfilled(task_id: &str, reason: &str) -> Self {
        Self {
            task_id: task_id.to_string(),
            backend_id: String::new(),
            ok: false,
            task_completed: false,
            output: String::new(),
            error: reason.to_string(),
        }
    }
}

/// Backend adapter around a chat-completions model provider.
struct ProviderBackend {
    provider: Arc<dyn ModelProvider>,
}

impl Backend for ProviderBackend {
    fn id(&self) -> &str {
        self.provider.name()
    }

    fn probe(&self) -> crate::backends::BackendHealth {
        crate::backends::BackendHealth {
            available: self.provider.is_available(),
            reliability: 0.9,
            latency_ms: 2000,
            estimated_cost_usd: 0.01,
        }
    }

    fn can_run(&self, _task: &TaskSpec) -> bool {
        self.provider.is_available()
    }

    fn execute(&self, task: &TaskSpec) -> ExecutionResult {
        match self
            .provider
            .chat("You are Ghost, a helpful assistant.", &task.objective)
        {
            Ok(output) => ExecutionResult {
                backend_id: self.id().to_string(),
                task_id: task.id.clone(),
                ok: true,
                output,
                error: String::new(),
                metrics: Default::default(),
            },
            Err(err) => ExecutionResult {
                backend_id: self.id().to_string(),
                task_id: task.id.clone(),
                ok: false,
                output: String::new(),
                error: err.to_string(),
                metrics: Default::default(),
            },
        }
    }
}

/// The pilot kernel.
pub struct ChimeraPilotKernel {
    backends: Vec<Box<dyn Backend>>,
    autonomy_level: String,
}

impl ChimeraPilotKernel {
    /// Build a kernel from options, mirroring `ChimeraPilotKernel.default()`.
    pub fn default_with(options: KernelOptions, provider: Option<Arc<dyn ModelProvider>>) -> Self {
        let mut backends: Vec<Box<dyn Backend>> = Vec::new();
        if options.include_deterministic_backend {
            let backend = if options.deterministic_fulfill {
                DeterministicBackend::fulfilling()
            } else {
                DeterministicBackend::new()
            };
            backends.push(Box::new(backend));
        }
        if options.include_model_provider_backend {
            if let Some(p) = provider {
                if p.is_available() {
                    backends.push(Box::new(ProviderBackend { provider: p }));
                }
            }
        }
        Self {
            backends,
            autonomy_level: options.autonomy_level,
        }
    }

    /// Compile an objective into tasks. Phase 2: one task per objective.
    pub fn compile(&self, objective: &str) -> Vec<TaskSpec> {
        vec![TaskSpec::new("task-1", TaskKind::Reasoning, objective)]
    }

    /// Run an objective through backend selection and execution.
    ///
    /// Backend order: deterministic first (when registered), then provider.
    /// The first backend whose `can_run()` passes executes the task. If the
    /// deterministic backend is in fulfill mode and cannot fulfill, the
    /// kernel tries the next backend before giving up honestly.
    pub fn run(&self, objective: &str) -> Result<Vec<PilotExecution>, GhostError> {
        let tasks = self.compile(objective);
        let mut executions = Vec::with_capacity(tasks.len());
        for task in &tasks {
            executions.push(self.execute_task(task));
        }
        Ok(executions)
    }

    fn execute_task(&self, task: &TaskSpec) -> PilotExecution {
        let mut last_unfulfilled: Option<String> = None;
        for backend in &self.backends {
            if !backend.can_run(task) {
                continue;
            }
            let result = backend.execute(task);
            if result.ok {
                return PilotExecution::from_result(result);
            }
            // Remember the honest reason; a later backend may still succeed.
            last_unfulfilled = Some(result.error.clone());
        }
        // No backend fulfilled the objective: honest failure, never false success.
        PilotExecution::unfulfilled(
            &task.id,
            &last_unfulfilled.unwrap_or_else(|| {
                format!(
                    "{UNFULFILLED_ERROR}: no backend available for task {}",
                    task.id
                )
            }),
        )
    }

    /// The autonomy level this kernel was built with.
    pub fn autonomy_level(&self) -> &str {
        &self.autonomy_level
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::deterministic::DeterministicBackend;

    fn fulfilling_kernel() -> ChimeraPilotKernel {
        ChimeraPilotKernel::default_with(
            KernelOptions {
                include_deterministic_backend: true,
                deterministic_fulfill: true,
                include_model_provider_backend: false,
                autonomy_level: "supervised".to_string(),
            },
            None,
        )
    }

    #[test]
    fn honest_success_for_fulfillable_objective() {
        let kernel = fulfilling_kernel();
        let executions = kernel.run("what is 2 + 3 * 4").expect("run");
        assert_eq!(executions.len(), 1);
        let exec = &executions[0];
        assert!(exec.ok);
        assert!(exec.task_completed);
        assert_eq!(exec.output, "14");
        assert_eq!(exec.backend_id, DeterministicBackend::ID);
        assert!(exec.error.is_empty());
    }

    #[test]
    fn honest_failure_for_unfulfillable_objective() {
        // PR #116: never false success.
        let kernel = fulfilling_kernel();
        let executions = kernel.run("write a poem about the ocean").expect("run");
        assert_eq!(executions.len(), 1);
        let exec = &executions[0];
        assert!(!exec.ok);
        assert!(!exec.task_completed);
        assert!(exec.output.is_empty());
        assert!(
            exec.error.starts_with(UNFULFILLED_ERROR),
            "got: {}",
            exec.error
        );
    }

    #[test]
    fn honest_failure_when_no_backends_registered() {
        let kernel = ChimeraPilotKernel::default_with(
            KernelOptions {
                include_deterministic_backend: false,
                deterministic_fulfill: false,
                include_model_provider_backend: false,
                autonomy_level: "supervised".to_string(),
            },
            None,
        );
        let executions = kernel.run("what is 1 + 1").expect("run");
        let exec = &executions[0];
        assert!(!exec.ok);
        assert!(!exec.task_completed);
        assert!(
            exec.error.starts_with(UNFULFILLED_ERROR),
            "got: {}",
            exec.error
        );
    }

    #[test]
    fn placeholder_mode_keeps_legacy_ok() {
        let kernel = ChimeraPilotKernel::default_with(KernelOptions::default(), None);
        let executions = kernel.run("anything at all").expect("run");
        assert!(executions[0].ok, "legacy placeholder mode stays ok");
    }

    #[test]
    fn execution_envelope_serializes() {
        let kernel = fulfilling_kernel();
        let executions = kernel.run("what is 1 + 1").expect("run");
        let json = serde_json::to_value(&executions[0]).expect("serialize");
        assert_eq!(json["ok"], true);
        assert_eq!(json["task_completed"], true);
        assert_eq!(json["output"], "2");
    }

    #[test]
    fn autonomy_level_is_exposed() {
        let kernel = fulfilling_kernel();
        assert_eq!(kernel.autonomy_level(), "supervised");
    }
}
