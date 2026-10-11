//! `TrustRuntimeStore`: persistent local storage for the Ghost trust runtime.
//!
//! Layout (mirrors Python):
//! ```text
//! <state_dir>/trust_runtime/
//!   runs/<run_id>.jsonl          # hash-chained step journal
//!   runs/<run_id>.tools.jsonl    # tool call records
//!   run_index.json               # run_id -> DurableRun
//! ```
//!
//! Journal integrity: every record carries `previous_hash` (the previous
//! record's `record_hash`, `""` for the first) and `record_hash`
//! (`sha256` of the canonical compact JSON of the record *without*
//! `record_hash`). [`TrustRuntimeStore::verify_run_integrity`] replays the
//! chain exactly like Python's `verify_run_integrity`.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ghost_core::{
    hash_payload, python_json, redact_text, redact_value, stable_id, GhostError, Result,
};
use serde_json::Value;

use crate::types::{
    DurableRun, ReplayComparison, ReplayExecution, ReplayReport, RunStepRecord, SafeSnippet,
};

/// A run with its journaled steps.
#[derive(Debug, Clone)]
pub struct RunView {
    pub run: DurableRun,
    pub steps: Vec<RunStepRecord>,
}

/// Result of [`TrustRuntimeStore::verify_run_integrity`].
#[derive(Debug, Clone, PartialEq)]
pub struct VerificationReport {
    pub ok: bool,
    pub verified: bool,
    pub run_id: String,
    pub step_count: usize,
    pub latest_hash: String,
    pub error: String,
    pub error_index: Option<usize>,
}

/// Persistent local storage for the Ghost trust runtime.
pub struct TrustRuntimeStore {
    state_dir: PathBuf,
    trust_dir: PathBuf,
    runs_dir: PathBuf,
    index_path: PathBuf,
    /// Guards index read-modify-write sequences (mirrors `_session_locks`).
    index_lock: Mutex<()>,
}

impl TrustRuntimeStore {
    /// Open (creating directories as needed) a store at `state_dir`.
    pub fn new(state_dir: impl Into<PathBuf>) -> Result<Self> {
        let state_dir = state_dir.into();
        let trust_dir = state_dir.join("trust_runtime");
        let runs_dir = trust_dir.join("runs");
        let index_path = trust_dir.join("run_index.json");
        fs::create_dir_all(&trust_dir)?;
        fs::create_dir_all(&runs_dir)?;
        Ok(Self {
            state_dir,
            trust_dir,
            runs_dir,
            index_path,
            index_lock: Mutex::new(()),
        })
    }

    /// Create a run and journal its `run_created` step.
    /// Mirrors `create_run(objective, source=..., agent_name=..., ...)`.
    pub fn create_run(
        &self,
        objective: &str,
        source: &str,
        agent_name: &str,
        metadata: Option<Value>,
    ) -> Result<DurableRun> {
        let now = ghost_core::now();
        // Python: _stable_id(source, objective, _now(), length=20).
        // The float formats via str(float) — shortest repr, same as Rust's {}.
        let run_id = stable_id(&[Some(source), Some(objective), Some(&now.to_string())], 20);
        let run = DurableRun {
            run_id: run_id.clone(),
            objective: objective.to_string(),
            source: source.to_string(),
            status: "running".to_string(),
            created_at: now,
            updated_at: now,
            agent_name: agent_name.to_string(),
            ghost_path: String::new(),
            model_provider: String::new(),
            model_name: String::new(),
            step_count: 0,
            pending_approval_count: 0,
            metadata: metadata.map(|m| redact_value(&m)),
        };
        {
            let _guard = self.index_lock.lock().unwrap();
            let mut index = self.load_index()?;
            index.insert(run_id.clone(), serde_json::to_value(&run)?);
            self.save_index(&index)?;
        }
        let created_key = format!("{run_id}:created");
        self.record_step(
            &run_id,
            RecordStepArgs {
                step_type: "run_created",
                status: "ok",
                input_payload: Some(serde_json::json!({"objective": objective, "source": source})),
                output_payload: None,
                policy_decision: None,
                idempotency_key: Some(&created_key),
                retryable: false,
                duration_ms: 0.0,
                error: "",
            },
        )?;
        Ok(run)
    }

    /// Journal a step, returning the stored record (with hash-chain fields).
    pub fn record_step(&self, run_id: &str, args: RecordStepArgs<'_>) -> Result<RunStepRecord> {
        // Idempotency: return the existing step if the key was seen.
        if let Some(key) = args.idempotency_key {
            if !key.is_empty() {
                if let Some(existing) = self.find_step_by_idempotency(run_id, key)? {
                    return Ok(existing);
                }
            }
        }
        let now = ghost_core::now();
        // Python: _stable_id(run_id, step_type, status, idempotency_key or _now(), length=18).
        let id_part = match args.idempotency_key {
            Some(k) if !k.is_empty() => k.to_string(),
            _ => now.to_string(),
        };
        let step_id = stable_id(
            &[
                Some(run_id),
                Some(args.step_type),
                Some(args.status),
                Some(&id_part),
            ],
            18,
        );
        // Python slices the redacted error to 500 chars.
        let error: String = redact_text(args.error).chars().take(500).collect();
        let mut record = RunStepRecord {
            run_id: run_id.to_string(),
            step_id,
            step_type: args.step_type.to_string(),
            status: args.status.to_string(),
            input_ref: args
                .input_payload
                .as_ref()
                .map(SafeSnippet::of)
                .unwrap_or_else(SafeSnippet::empty),
            output_ref: args
                .output_payload
                .as_ref()
                .map(SafeSnippet::of)
                .unwrap_or_else(SafeSnippet::empty),
            policy_decision: args.policy_decision.cloned().unwrap_or(Value::Null),
            idempotency_key: args.idempotency_key.unwrap_or("").to_string(),
            retryable: args.retryable,
            timestamp: now,
            duration_ms: args.duration_ms,
            error,
            previous_hash: String::new(),
            record_hash: String::new(),
        };
        // policy_decision is redacted in Python's to_dict().
        record.policy_decision = redact_value(&record.policy_decision);

        // Hash chain: previous_hash = last record's record_hash ("" if none).
        let journal_path = self.journal_path(run_id);
        let previous_hash = self
            .read_journal(&journal_path)?
            .last()
            .map(|r| r.record_hash.clone())
            .unwrap_or_default();
        record.previous_hash = previous_hash;
        // record_hash = sha256(canonical_json(record without record_hash)).
        let mut hash_input = serde_json::to_value(&record)?;
        if let Value::Object(ref mut map) = hash_input {
            map.remove("record_hash");
        }
        record.record_hash = hash_payload(&hash_input);

        // Append the redacted record as one JSON line (Python spaced format).
        let line_value = redact_value(&serde_json::to_value(&record)?);
        let line = python_json(&line_value);
        self.append_line(&journal_path, &line)?;

        self.touch_run(run_id, args.status, true)?;
        Ok(record)
    }

    /// Load a run and its journaled steps. Returns `Ok(None)` when unknown.
    pub fn get_run(&self, run_id: &str) -> Result<Option<RunView>> {
        let _guard = self.index_lock.lock().unwrap();
        let index = self.load_index()?;
        let Some(run_value) = index.get(run_id) else {
            return Ok(None);
        };
        let run: DurableRun = serde_json::from_value(run_value.clone())?;
        let steps = self.read_journal(&self.journal_path(run_id))?;
        Ok(Some(RunView { run, steps }))
    }

    /// List all runs, newest first.
    pub fn list_runs(&self) -> Result<Vec<DurableRun>> {
        let _guard = self.index_lock.lock().unwrap();
        let index = self.load_index()?;
        let mut runs: Vec<DurableRun> = index
            .values()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();
        runs.sort_by(|a, b| {
            b.created_at
                .partial_cmp(&a.created_at)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(runs)
    }

    /// Verify a run's journal hash chain. Mirrors `verify_run_integrity`.
    pub fn verify_run_integrity(&self, run_id: &str) -> Result<VerificationReport> {
        let steps = self.read_journal(&self.journal_path(run_id))?;
        let mut previous_hash = String::new();
        for (index, step) in steps.iter().enumerate() {
            if step.previous_hash != previous_hash {
                return Ok(VerificationReport {
                    ok: false,
                    verified: false,
                    run_id: run_id.to_string(),
                    step_count: steps.len(),
                    latest_hash: previous_hash,
                    error: "previous_hash mismatch".to_string(),
                    error_index: Some(index),
                });
            }
            let mut hash_input = serde_json::to_value(step)?;
            if let Value::Object(ref mut map) = hash_input {
                map.remove("record_hash");
            }
            let actual = hash_payload(&hash_input);
            if step.record_hash != actual {
                return Ok(VerificationReport {
                    ok: false,
                    verified: false,
                    run_id: run_id.to_string(),
                    step_count: steps.len(),
                    latest_hash: previous_hash,
                    error: "record_hash mismatch".to_string(),
                    error_index: Some(index),
                });
            }
            previous_hash = step.record_hash.clone();
        }
        Ok(VerificationReport {
            ok: true,
            verified: true,
            run_id: run_id.to_string(),
            step_count: steps.len(),
            latest_hash: previous_hash,
            error: String::new(),
            error_index: None,
        })
    }

    /// The base state directory.
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Re-execute a journaled run's objective and compare outcomes.
    ///
    /// Port of Python's `replay_run` (PR #117, real re-execution — bug #6).
    /// Creates a new run linked to the original (`source="replay"`,
    /// `metadata={"replayed_from": run_id}`), runs `executor` against the
    /// objective, journals one `replay_execution` step per execution plus a
    /// `replay_comparison` step, and returns the outcome comparison.
    ///
    /// Returns [`GhostError::NotFound`] when `run_id` is unknown and
    /// [`GhostError::Config`] when the original run has no objective.
    /// When the executor itself fails, a `replay_failed` step is journaled
    /// and the original error is returned.
    pub fn replay_run(
        &self,
        run_id: &str,
        executor: &dyn Fn(&str) -> Result<Vec<ReplayExecution>>,
    ) -> Result<ReplayReport> {
        let view = match self.get_run(run_id)? {
            Some(v) => v,
            None => {
                return Err(GhostError::NotFound(format!("run not found: {run_id}")));
            }
        };
        let objective = view.run.objective.trim().to_string();
        if objective.is_empty() {
            return Err(GhostError::Config(
                "Original run has no recorded objective to replay.".to_string(),
            ));
        }
        let metadata = serde_json::json!({"replayed_from": run_id});
        let replay_run = self.create_run(&objective, "replay", "ghost-chimera", Some(metadata))?;
        let replay_id = replay_run.run_id.clone();

        let executions = match executor(&objective) {
            Ok(executions) => executions,
            Err(err) => {
                self.record_step(
                    &replay_id,
                    RecordStepArgs {
                        step_type: "replay_failed",
                        status: "error",
                        input_payload: Some(
                            serde_json::json!({"replayed_from": run_id, "objective": objective}),
                        ),
                        output_payload: Some(serde_json::json!({"error": err.to_string()})),
                        policy_decision: None,
                        idempotency_key: None,
                        retryable: false,
                        duration_ms: 0.0,
                        error: &err.to_string(),
                    },
                )?;
                return Err(err);
            }
        };

        for execution in &executions {
            let output: String = execution.output.chars().take(2000).collect();
            self.record_step(
                &replay_id,
                RecordStepArgs {
                    step_type: "replay_execution",
                    status: if execution.ok { "ok" } else { "error" },
                    input_payload: Some(
                        serde_json::json!({"replayed_from": run_id, "task_id": execution.task_id}),
                    ),
                    output_payload: Some(serde_json::json!({
                        "ok": execution.ok,
                        "backend_id": execution.backend_id,
                        "output": output,
                        "error": execution.error,
                    })),
                    policy_decision: None,
                    idempotency_key: None,
                    retryable: false,
                    duration_ms: 0.0,
                    error: &execution.error,
                },
            )?;
        }

        let original_ok_steps = view
            .steps
            .iter()
            .filter(|s| matches!(s.status.as_str(), "ok" | "completed"))
            .count();
        let original_error_steps = view
            .steps
            .iter()
            .filter(|s| matches!(s.status.as_str(), "error" | "failed" | "denied" | "blocked"))
            .count();
        let replay_ok = executions.iter().filter(|e| e.ok).count();
        let replay_errors = executions.len().saturating_sub(replay_ok);
        // Empty executions count as not-all-ok (mirrors Python's `ok = ... if executions else False`).
        let all_ok = !executions.is_empty() && replay_ok == executions.len();
        let outcome_match = (original_error_steps == 0) == all_ok;
        let comparison = ReplayComparison {
            original_ok_steps,
            original_error_steps,
            replay_ok,
            replay_errors,
            outcome_match,
        };

        self.record_step(
            &replay_id,
            RecordStepArgs {
                step_type: "replay_comparison",
                status: "ok",
                input_payload: Some(serde_json::json!({"replayed_from": run_id})),
                output_payload: Some(serde_json::to_value(&comparison)?),
                policy_decision: None,
                idempotency_key: None,
                retryable: false,
                duration_ms: 0.0,
                error: "",
            },
        )?;

        Ok(ReplayReport {
            replay_run_id: replay_id,
            replayed_from: run_id.to_string(),
            objective,
            execution_performed: true,
            executions,
            comparison,
        })
    }

    /// The trust-runtime directory (`<state_dir>/trust_runtime`).
    pub fn trust_dir(&self) -> &Path {
        &self.trust_dir
    }

    // -- internals ------------------------------------------------------

    fn journal_path(&self, run_id: &str) -> PathBuf {
        self.runs_dir.join(format!("{run_id}.jsonl"))
    }

    fn load_index(&self) -> Result<BTreeMap<String, Value>> {
        if !self.index_path.exists() {
            return Ok(BTreeMap::new());
        }
        let text = fs::read_to_string(&self.index_path)?;
        let map: BTreeMap<String, Value> = serde_json::from_str(&text).unwrap_or_default();
        Ok(map)
    }

    fn save_index(&self, index: &BTreeMap<String, Value>) -> Result<()> {
        // Atomic write via temp file + rename (mirrors _write_jsonl).
        let tmp = self.index_path.with_extension("json.tmp");
        let text = python_json(&serde_json::to_value(index)?);
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &self.index_path)?;
        Ok(())
    }

    fn read_journal(&self, path: &Path) -> Result<Vec<RunStepRecord>> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        let text = fs::read_to_string(path)?;
        let mut out = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // Skip corrupt lines, like Python's _read_jsonl.
            if let Ok(mut value) = serde_json::from_str::<Value>(line) {
                // Redact on read, mirroring _read_jsonl.
                value = redact_value(&value);
                if let Ok(record) = serde_json::from_value(value) {
                    out.push(record);
                }
            }
        }
        Ok(out)
    }

    fn append_line(&self, path: &Path, line: &str) -> Result<()> {
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(file, "{line}")?;
        file.sync_all()?;
        Ok(())
    }

    fn find_step_by_idempotency(&self, run_id: &str, key: &str) -> Result<Option<RunStepRecord>> {
        for step in self.read_journal(&self.journal_path(run_id))? {
            if step.idempotency_key == key {
                return Ok(Some(step));
            }
        }
        Ok(None)
    }

    fn touch_run(&self, run_id: &str, status: &str, increment_step: bool) -> Result<()> {
        let _guard = self.index_lock.lock().unwrap();
        let mut index = self.load_index()?;
        let Some(run_value) = index.get_mut(run_id) else {
            return Ok(());
        };
        if let Value::Object(map) = run_value {
            map.insert("updated_at".into(), Value::from(ghost_core::now()));
            if !status.is_empty() {
                map.insert("status".into(), Value::String(status.to_string()));
            }
            if increment_step {
                let count = map.get("step_count").and_then(Value::as_i64).unwrap_or(0);
                map.insert("step_count".into(), Value::from(count + 1));
            }
        }
        self.save_index(&index)?;
        Ok(())
    }
}

/// Arguments for [`TrustRuntimeStore::record_step`].
#[derive(Debug, Clone)]
pub struct RecordStepArgs<'a> {
    pub step_type: &'a str,
    pub status: &'a str,
    pub input_payload: Option<Value>,
    pub output_payload: Option<Value>,
    pub policy_decision: Option<&'a Value>,
    pub idempotency_key: Option<&'a str>,
    pub retryable: bool,
    pub duration_ms: f64,
    pub error: &'a str,
}

impl<'a> RecordStepArgs<'a> {
    /// Minimal args for a simple status step.
    pub fn simple(step_type: &'a str, status: &'a str) -> Self {
        Self {
            step_type,
            status,
            input_payload: None,
            output_payload: None,
            policy_decision: None,
            idempotency_key: None,
            retryable: false,
            duration_ms: 0.0,
            error: "",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> (tempfile::TempDir, TrustRuntimeStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = TrustRuntimeStore::new(dir.path()).unwrap();
        (dir, store)
    }

    #[test]
    fn create_run_journals_run_created() {
        let (_dir, store) = test_store();
        let run = store
            .create_run("test objective", "ask-cli", "ghost-chimera", None)
            .unwrap();
        assert_eq!(run.objective, "test objective");
        assert_eq!(run.source, "ask-cli");
        assert_eq!(run.run_id.len(), 20);

        let view = store.get_run(&run.run_id).unwrap().unwrap();
        assert_eq!(view.steps.len(), 1);
        assert_eq!(view.steps[0].step_type, "run_created");
        assert_eq!(view.steps[0].status, "ok");
    }

    #[test]
    fn record_step_chains_hashes() {
        let (_dir, store) = test_store();
        let run = store
            .create_run("obj", "console", "ghost-chimera", None)
            .unwrap();
        // create_run journals run_created first.
        let created_hash = store
            .get_run(&run.run_id)
            .unwrap()
            .unwrap()
            .steps
            .into_iter()
            .next()
            .unwrap()
            .record_hash;
        let s1 = store
            .record_step(&run.run_id, RecordStepArgs::simple("step_a", "ok"))
            .unwrap();
        let s2 = store
            .record_step(&run.run_id, RecordStepArgs::simple("step_b", "ok"))
            .unwrap();
        assert_eq!(s1.previous_hash, created_hash);
        assert!(!s1.record_hash.is_empty());
        assert_eq!(s2.previous_hash, s1.record_hash);

        let report = store.verify_run_integrity(&run.run_id).unwrap();
        assert!(report.ok && report.verified);
        assert_eq!(report.step_count, 3); // run_created + 2
        assert_eq!(report.latest_hash, s2.record_hash);
    }

    #[test]
    fn tampered_journal_fails_verification() {
        let (_dir, store) = test_store();
        let run = store
            .create_run("obj", "console", "ghost-chimera", None)
            .unwrap();
        store
            .record_step(&run.run_id, RecordStepArgs::simple("step_a", "ok"))
            .unwrap();

        // Tamper with the journal file directly.
        let path = store.journal_path(&run.run_id);
        let text = fs::read_to_string(&path).unwrap();
        let tampered = text.replace("\"step_a\"", "\"step_EVIL\"");
        fs::write(&path, tampered).unwrap();

        let report = store.verify_run_integrity(&run.run_id).unwrap();
        assert!(!report.ok);
        assert_eq!(report.error, "record_hash mismatch");
    }

    #[test]
    fn idempotency_returns_existing_step() {
        let (_dir, store) = test_store();
        let run = store
            .create_run("obj", "console", "ghost-chimera", None)
            .unwrap();
        let args = RecordStepArgs {
            idempotency_key: Some("unique-key"),
            ..RecordStepArgs::simple("step_a", "ok")
        };
        let s1 = store.record_step(&run.run_id, args.clone()).unwrap();
        let s2 = store.record_step(&run.run_id, args).unwrap();
        assert_eq!(s1.step_id, s2.step_id);

        let view = store.get_run(&run.run_id).unwrap().unwrap();
        // run_created + 1 (second call was a no-op).
        assert_eq!(view.steps.len(), 2);
    }

    #[test]
    fn error_text_redacted_and_truncated() {
        let (_dir, store) = test_store();
        let run = store
            .create_run("obj", "console", "ghost-chimera", None)
            .unwrap();
        let long_error = format!("key sk-{} {}", "a".repeat(20), "x".repeat(1000));
        let step = store
            .record_step(
                &run.run_id,
                RecordStepArgs {
                    error: &long_error,
                    ..RecordStepArgs::simple("step_a", "error")
                },
            )
            .unwrap();
        assert!(!step.error.contains("sk-"));
        assert!(step.error.contains("[redacted]"));
        assert!(step.error.chars().count() <= 500);
    }

    #[test]
    fn list_runs_newest_first() {
        let (_dir, store) = test_store();
        let r1 = store
            .create_run("first", "console", "ghost-chimera", None)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let r2 = store
            .create_run("second", "console", "ghost-chimera", None)
            .unwrap();
        let runs = store.list_runs().unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].run_id, r2.run_id);
        assert_eq!(runs[1].run_id, r1.run_id);
    }

    #[test]
    fn get_run_unknown_returns_none() {
        let (_dir, store) = test_store();
        assert!(store.get_run("nope").unwrap().is_none());
    }

    #[test]
    fn step_count_increments_in_index() {
        let (_dir, store) = test_store();
        let run = store
            .create_run("obj", "console", "ghost-chimera", None)
            .unwrap();
        store
            .record_step(&run.run_id, RecordStepArgs::simple("s", "ok"))
            .unwrap();
        let runs = store.list_runs().unwrap();
        // run_created + 1 recorded step.
        assert_eq!(runs[0].step_count, 2);
    }

    fn ok_executor(output: &str) -> impl Fn(&str) -> Result<Vec<ReplayExecution>> + '_ {
        let output = output.to_string();
        move |_objective: &str| {
            Ok(vec![ReplayExecution {
                task_id: "task-1".to_string(),
                backend_id: "deterministic".to_string(),
                ok: true,
                output: output.clone(),
                error: String::new(),
            }])
        }
    }

    #[test]
    fn replay_success_journals_execution_and_comparison() {
        let (_dir, store) = test_store();
        let run = store
            .create_run("what is 6 * 7", "console", "ghost-chimera", None)
            .unwrap();
        let report = store.replay_run(&run.run_id, &ok_executor("42")).unwrap();

        assert_eq!(report.replayed_from, run.run_id);
        assert_eq!(report.objective, "what is 6 * 7");
        assert!(report.execution_performed);
        assert_ne!(report.replay_run_id, run.run_id);
        assert_eq!(report.executions.len(), 1);
        assert_eq!(report.comparison.replay_ok, 1);
        assert_eq!(report.comparison.replay_errors, 0);
        // Original had only run_created (ok) and no error steps; replay fully ok.
        assert_eq!(report.comparison.original_ok_steps, 1);
        assert_eq!(report.comparison.original_error_steps, 0);
        assert!(report.comparison.outcome_match);

        // The replay run is journaled: run_created + replay_execution + replay_comparison.
        let view = store.get_run(&report.replay_run_id).unwrap().unwrap();
        assert_eq!(view.run.source, "replay");
        assert_eq!(view.run.objective, "what is 6 * 7");
        assert_eq!(
            view.run.metadata.as_ref().unwrap()["replayed_from"],
            serde_json::json!(run.run_id)
        );
        let step_types: Vec<&str> = view.steps.iter().map(|s| s.step_type.as_str()).collect();
        assert_eq!(
            step_types,
            vec!["run_created", "replay_execution", "replay_comparison"]
        );
        assert!(
            store
                .verify_run_integrity(&report.replay_run_id)
                .unwrap()
                .ok
        );
    }

    #[test]
    fn replay_unknown_run_returns_not_found() {
        let (_dir, store) = test_store();
        let err = store
            .replay_run("no-such-run", &ok_executor("42"))
            .expect_err("must fail");
        assert!(
            matches!(err, GhostError::NotFound(_)),
            "expected NotFound, got: {err}"
        );
    }

    #[test]
    fn replay_empty_objective_is_an_error() {
        let (_dir, store) = test_store();
        let run = store
            .create_run("", "console", "ghost-chimera", None)
            .unwrap();
        let err = store
            .replay_run(&run.run_id, &ok_executor("42"))
            .expect_err("must fail");
        assert!(
            matches!(err, GhostError::Config(_)),
            "expected Config, got: {err}"
        );
    }

    #[test]
    fn replay_executor_error_records_replay_failed() {
        let (_dir, store) = test_store();
        let run = store
            .create_run("do the thing", "console", "ghost-chimera", None)
            .unwrap();
        let failing = |_objective: &str| -> Result<Vec<ReplayExecution>> {
            Err(GhostError::Unfulfilled(
                "deterministic_unfulfilled: boom".to_string(),
            ))
        };
        let err = store
            .replay_run(&run.run_id, &failing)
            .expect_err("must fail");
        assert!(err.to_string().contains("deterministic_unfulfilled"));

        // The replay run exists and journals the failure.
        let replay = store.list_runs().unwrap();
        let replay_run = replay.iter().find(|r| r.source == "replay").unwrap();
        let view = store.get_run(&replay_run.run_id).unwrap().unwrap();
        let failed: Vec<&RunStepRecord> = view
            .steps
            .iter()
            .filter(|s| s.step_type == "replay_failed")
            .collect();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].status, "error");
    }

    #[test]
    fn replay_unfulfillable_objective_is_honest() {
        let (_dir, store) = test_store();
        let run = store
            .create_run(
                "write a poem about the ocean",
                "console",
                "ghost-chimera",
                None,
            )
            .unwrap();
        let honest = |_objective: &str| -> Result<Vec<ReplayExecution>> {
            Ok(vec![ReplayExecution {
                task_id: "task-1".to_string(),
                backend_id: String::new(),
                ok: false,
                output: String::new(),
                error: "deterministic_unfulfilled: no backend available".to_string(),
            }])
        };
        let report = store.replay_run(&run.run_id, &honest).unwrap();
        assert_eq!(report.comparison.replay_ok, 0);
        assert_eq!(report.comparison.replay_errors, 1);
        // Original had no error steps, replay was not all-ok: outcomes differ.
        assert!(!report.comparison.outcome_match);
    }

    #[test]
    fn replay_comparison_counts_statuses() {
        let (_dir, store) = test_store();
        let run = store
            .create_run("mixed run", "console", "ghost-chimera", None)
            .unwrap();
        store
            .record_step(&run.run_id, RecordStepArgs::simple("s1", "completed"))
            .unwrap();
        store
            .record_step(&run.run_id, RecordStepArgs::simple("s2", "denied"))
            .unwrap();
        store
            .record_step(&run.run_id, RecordStepArgs::simple("s3", "blocked"))
            .unwrap();
        let mixed = |_objective: &str| -> Result<Vec<ReplayExecution>> {
            Ok(vec![
                ReplayExecution {
                    task_id: "t1".into(),
                    backend_id: "b".into(),
                    ok: true,
                    output: "x".into(),
                    error: String::new(),
                },
                ReplayExecution {
                    task_id: "t2".into(),
                    backend_id: "b".into(),
                    ok: false,
                    output: String::new(),
                    error: "nope".into(),
                },
            ])
        };
        let report = store.replay_run(&run.run_id, &mixed).unwrap();
        // run_created(ok) + completed = 2 ok; denied + blocked = 2 error.
        assert_eq!(report.comparison.original_ok_steps, 2);
        assert_eq!(report.comparison.original_error_steps, 2);
        assert_eq!(report.comparison.replay_ok, 1);
        assert_eq!(report.comparison.replay_errors, 1);
        // Original had error steps and replay was not all-ok: match.
        assert!(report.comparison.outcome_match);

        // Output payloads truncate at 2000 chars.
        let long = "y".repeat(5000);
        let long_exec = move |_objective: &str| -> Result<Vec<ReplayExecution>> {
            Ok(vec![ReplayExecution {
                task_id: "t".into(),
                backend_id: "b".into(),
                ok: true,
                output: long.clone(),
                error: String::new(),
            }])
        };
        let report2 = store.replay_run(&run.run_id, &long_exec).unwrap();
        let view = store.get_run(&report2.replay_run_id).unwrap().unwrap();
        let exec_step = view
            .steps
            .iter()
            .find(|s| s.step_type == "replay_execution")
            .unwrap();
        assert!(exec_step.output_ref.preview.contains("yyy"));
    }
}
